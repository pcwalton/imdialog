// imdialog/src/main.rs

#![feature(link_args)]

extern crate clap;
extern crate gl;
extern crate libc;
extern crate imgui_sys;
extern crate num;
extern crate sdl2;
extern crate xdg;

#[cfg(target_os="linux")]
extern crate ioctl_rs as ioctl;

use clap::{App, Arg, Values};
use imgui_sys as imgui;
use imgui_sys::{ImDrawData, ImDrawIdx, ImDrawVert, ImFont, ImGuiSelectableFlags, ImGuiSetCond};
use imgui_sys::{ImVec2, ImVec4};
use libc::{c_char, c_int, c_uchar, c_uint, intptr_t};
use num::ToPrimitive;
use sdl2::Sdl;
use sdl2::event::Event;
use sdl2::keyboard::{self, Scancode};
use sdl2::video::Window;
use std::cmp::Ordering;
use std::ffi::{CStr, CString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::mem;
use std::os::raw::c_void;
use std::path::{Path, PathBuf};
use std::process;
use std::ptr;

#[cfg(unix)]
use xdg::BaseDirectories;

#[cfg(target_arch="arm")]
const FRAMEBUFFER_WIDTH: u32 = 1920;
#[cfg(target_arch="arm")]
const FRAMEBUFFER_HEIGHT: u32 = 1080;
#[cfg(not(target_arch="arm"))]
const FRAMEBUFFER_WIDTH: u32 = 800;
#[cfg(not(target_arch="arm"))]
const FRAMEBUFFER_HEIGHT: u32 = 600;

#[cfg(target_os="linux")]
const K_XLATE: c_int = 0x01;

const LIST_HEIGHT: c_int = 5;

const MAX_TEXT_LENGTH: usize = 1024;

static FONT_FILENAME: &'static str = "Muli.ttf";
static STANDARD_FONT_SIZE: f32 = (FRAMEBUFFER_HEIGHT as f32) / 16.66666;
static LABEL_FONT_SIZE: f32 = (FRAMEBUFFER_HEIGHT as f32) / 25.0;

static ZERO_VERTEX: ImDrawVert = ImDrawVert {
    pos: ImVec2 { x: 0.0, y: 0.0, },
    uv: ImVec2 { x: 0.0, y: 0.0, },
    col: 0,
};

static ZERO_SIZE: ImVec2 = ImVec2 {
    x: 0.0,
    y: 0.0,
};

static LABEL_COLOR: ImVec4 = ImVec4 {
    x: 0.5,
    y: 0.5,
    z: 0.5,
    w: 1.0,
};

static mut RENDERER: *const Renderer = 0 as *const Renderer;

static SCANCODES: [Scancode; 19] = [
    Scancode::Tab,
    Scancode::Left,
    Scancode::Right,
    Scancode::Up,
    Scancode::Down,
    Scancode::PageUp,
    Scancode::PageDown,
    Scancode::Home,
    Scancode::End,
    Scancode::Delete,
    Scancode::Backspace,
    Scancode::Return,
    Scancode::Escape,
    Scancode::A,
    Scancode::C,
    Scancode::V,
    Scancode::X,
    Scancode::Y,
    Scancode::Z,
];

fn button_size() -> ImVec2 {
    ImVec2 {
        x: FRAMEBUFFER_WIDTH.to_pixels() * 0.8,
        y: 0.0,
    }
}

trait ToPixels {
    fn to_pixels(&self) -> f32;
}

impl ToPixels for u32 {
    fn to_pixels(&self) -> f32 {
        *self as f32
    }
}

#[cfg(not(unix))]
struct BaseDirectories;

#[cfg(not(unix))]
#[derive(Debug)]
struct BaseDirectoriesError;

#[cfg(not(unix))]
impl BaseDirectories {
    fn with_prefix<P>(_: P) -> Result<BaseDirectories, BaseDirectoriesError> where P: AsRef<Path> {
        Ok(BaseDirectories)
    }

    fn find_data_file<P>(&self, _: P) -> Option<PathBuf> where P: AsRef<Path> {
        None
    }
}

struct Shader(c_uint);

fn get_data_file_path(filename: &str, base_directories: &BaseDirectories) -> PathBuf {
    match base_directories.find_data_file(Path::new(filename)) {
        Some(path) => return path,
        None => {}
    }

    let path = PathBuf::from(filename);
    if path.exists() {
        return path
    }

    writeln!(io::stderr(),
             "error: couldn't find data file `{}`: try installing it to \
              `~/.local/share/imdialog/{}` or `/usr/local/share/imdialog/{}`",
             filename,
             filename,
             filename).unwrap();
    process::exit(0);
}

impl Shader {
    pub fn new(filename: &str, kind: c_uint, base_directories: &BaseDirectories) -> Shader {
        let path = get_data_file_path(filename, base_directories);

        let mut shader_source = String::new();
        File::open(path).unwrap().read_to_string(&mut shader_source).unwrap();
        let shader_source = CString::new(shader_source).unwrap();

        unsafe {
            let shader = gl::CreateShader(kind);
            gl::ShaderSource(shader, 1, &shader_source.as_ptr(), ptr::null());
            gl::CompileShader(shader);
            Shader(shader)
        }
    }
}

struct MenuItem {
    tag: String,
    item: String,
}

struct FileDialogEntries {
    entries: Vec<*const c_char>,
    index: c_int,
}

impl Drop for FileDialogEntries {
    fn drop(&mut self) {
        unsafe {
            for &entry in &self.entries {
                libc::free(entry as *mut _)
            }
        }
    }
}

impl FileDialogEntries {
    fn new(path: &Path) -> FileDialogEntries {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(_) => return FileDialogEntries::none(),
        };
        if !metadata.is_dir() {
            return FileDialogEntries::none()
        }
        let directory_entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(_) => return FileDialogEntries::none(),
        };

        unsafe {
            let mut entries = vec![];
            for entry in directory_entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => continue,
                };
                let path = entry.path();
                let filename = match path.file_name() {
                    Some(filename) => filename,
                    None => continue,
                };
                let mut string = match filename.to_str() {
                    Some(string) => string.to_string(),
                    None => continue,
                };
                if path.is_dir() {
                    string.push_str("/")
                }
                let c_string = match CString::new(string) {
                    Ok(c_string) => c_string,
                    Err(_) => continue,
                };
                entries.push(libc::strdup(c_string.as_ptr()) as *const c_char)
            }

            entries.sort_by(|&a, &b| {
                match libc::strcmp(a, b) {
                    0 => Ordering::Equal,
                    x if x < 0 => Ordering::Less,
                    _ => Ordering::Greater,
                }
            });

            if path.parent().is_some() {
                let c_string = libc::strdup(b"Up one level\0" as *const c_uchar as *const c_char);
                entries.insert(0, c_string as *const c_char)
            }
            FileDialogEntries {
                entries: entries,
                index: 0,
            }
        }
    }

    fn none() -> FileDialogEntries {
        FileDialogEntries {
            entries: vec![],
            index: 0,
        }
    }
}

#[derive(Copy, Clone, PartialEq)]
enum SelectedFileType {
    File,
    Directory,
}

struct FileDialog {
    path: PathBuf,
    entries: FileDialogEntries,
}

impl FileDialog {
    fn selected_path(&self) -> (PathBuf, SelectedFileType) {
        unsafe {
            let index = self.entries.index as usize;
            let mut entry_string = CStr::from_ptr(self.entries.entries[index]).to_str()
                                                                              .unwrap()
                                                                              .to_string();
            let mut path = self.path.clone();
            let file_type = if entry_string.ends_with("/") {
                entry_string.pop();
                SelectedFileType::Directory
            } else {
                SelectedFileType::File
            };
            path.push(entry_string);
            (path, file_type)
        }
    }
}

struct InputDialog {
    text: String,
    data: Vec<u8>,
}

#[allow(dead_code)]
struct MenuDialog {
    text: String,
    menu_height: u32,
    items: Vec<MenuItem>,
}

enum Subdialog {
    File(FileDialog),
    Input(InputDialog),
    Menu(MenuDialog),
}

fn usage(help_string: &[u8]) -> ! {
    io::stdout().write_all(&help_string).unwrap();
    io::stdout().write_all(b"\n").unwrap();
    process::exit(0)
}

#[allow(dead_code)]
struct Dialog {
    width: u32,
    height: u32,
    subdialog: Subdialog,
}

impl Dialog {
    fn new() -> Dialog {
        let app = App::new("imdialog").version("0.1")
                                      .author("Patrick Walton <pcwalton@mimiga.net>")
                                      .about("Display dialogs using IMGUI")
                                      .arg(Arg::with_name("fselect").long("fselect")
                                                                    .takes_value(true)
                                                                    .number_of_values(3))
                                      .arg(Arg::with_name("inputbox").long("inputbox")
                                                                     .takes_value(true)
                                                                     .min_values(3)
                                                                     .max_values(4))
                                      .arg(Arg::with_name("menu").long("menu")
                                                                 .takes_value(true)
                                                                 .min_values(3));

        let mut help_string = vec![];
        app.write_help(&mut help_string).unwrap();
        let matches = app.get_matches();

        if let Some(values) = matches.values_of("fselect") {
            return Dialog::fselect(values)
        }
        if let Some(values) = matches.values_of("inputbox") {
            return Dialog::inputbox(values)
        }
        if let Some(values) = matches.values_of("menu") {
            if let Some(menu) = Dialog::menu(values) {
                return menu
            }
        }

        usage(&help_string)
    }

    fn fselect(mut values: Values) -> Dialog {
        let path = fs::canonicalize(Path::new(values.next().unwrap())).unwrap();
        let width: u32 = values.next().unwrap().parse().unwrap();
        let height: u32 = values.next().unwrap().parse().unwrap();
        let entries = FileDialogEntries::new(&path);
        Dialog {
            width: width,
            height: height,
            subdialog: Subdialog::File(FileDialog {
                path: path,
                entries: entries,
            }),
        }
    }

    fn inputbox(mut values: Values) -> Dialog {
        let text = values.next().unwrap();
        let width: u32 = values.next().unwrap().parse().unwrap();
        let height: u32 = values.next().unwrap().parse().unwrap();

        let mut data = vec![];
        if let Some(initial_data) = values.next() {
            io::copy(&mut CString::new(initial_data).unwrap().as_bytes_with_nul(),
                     &mut data).unwrap();
        }
        data.resize(MAX_TEXT_LENGTH - 1, 0);
        data.push(0);

        Dialog {
            width: width,
            height: height,
            subdialog: Subdialog::Input(InputDialog {
                text: text.to_string(),
                data: data,
            }),
        }
    }

    fn menu(mut values: Values) -> Option<Dialog> {
        let text = values.next().unwrap();
        let width: u32 = values.next().unwrap().parse().unwrap();
        let height: u32 = values.next().unwrap().parse().unwrap();
        let menu_height: u32 = values.next().unwrap().parse().unwrap();

        let mut items = vec![];
        loop {
            let tag = match values.next() {
                Some(tag) => tag,
                None => break,
            };
            let item = match values.next() {
                Some(item) => item,
                None => return None,
            };
            items.push(MenuItem {
                tag: tag.to_string(),
                item: item.to_string(),
            })
        }

        Some(Dialog {
            width: width,
            height: height,
            subdialog: Subdialog::Menu(MenuDialog {
                text: text.to_string(),
                menu_height: menu_height,
                items: items,
            })
        })
    }
}

#[allow(dead_code)]
struct Renderer {
    standard_font: *mut ImFont,
    label_font: *mut ImFont,
    texture: c_uint,
    vertex_shader: Shader,
    fragment_shader: Shader,
    program: c_uint,
    u_window_size: c_int,
    u_texture: c_int,
    a_position: c_int,
    a_texture_uv: c_int,
    a_color: c_int,
    vbo: c_uint,
}

impl Renderer {
    fn new(base_directories: &BaseDirectories) -> Renderer {
        unsafe {
            let io = imgui::igGetIO();
            let data_file_path = get_data_file_path(FONT_FILENAME, base_directories).to_str()
                                                                                    .unwrap()
                                                                                    .to_string();
            let data_file_path = CString::new(data_file_path).unwrap();
            let standard_font = imgui::ImFontAtlas_AddFontFromFileTTF((*io).fonts,
                                                                      data_file_path.as_ptr(),
                                                                      STANDARD_FONT_SIZE,
                                                                      ptr::null(),
                                                                      ptr::null());
            let label_font = imgui::ImFontAtlas_AddFontFromFileTTF((*io).fonts,
                                                                   data_file_path.as_ptr(),
                                                                   LABEL_FONT_SIZE,
                                                                   ptr::null(),
                                                                   ptr::null());

            init_keys();
            let texture = init_texture();

            let vertex_shader = Shader::new("imgui.vs.glsl", gl::VERTEX_SHADER, base_directories);
            let fragment_shader = Shader::new("imgui.fs.glsl",
                                              gl::FRAGMENT_SHADER,
                                              base_directories);
            let program = gl::CreateProgram();
            gl::AttachShader(program, vertex_shader.0);
            gl::AttachShader(program, fragment_shader.0);
            gl::LinkProgram(program);
            gl::UseProgram(program);

            let u_window_size =
                gl::GetUniformLocation(program,
                                       b"uWindowSize\0" as *const c_uchar as *const c_char);
            let u_texture =
                gl::GetUniformLocation(program, b"uTexture\0" as *const c_uchar as *const c_char);
            let a_position =
                gl::GetAttribLocation(program, b"aPosition\0" as *const c_uchar as *const c_char);
            let a_texture_uv =
                gl::GetAttribLocation(program, b"aTextureUV\0" as *const c_uchar as *const c_char);
            let a_color = gl::GetAttribLocation(program,
                                                b"aColor\0" as *const c_uchar as *const c_char);

            let mut vbo = 0;
            gl::GenBuffers(1, &mut vbo);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

            let offset_of = |field_addr| {
                (field_addr - (&ZERO_VERTEX as *const _ as usize)) as *const c_void
            };

            gl::VertexAttribPointer(a_position as c_uint,
                                    2,
                                    gl::FLOAT,
                                    gl::FALSE,
                                    mem::size_of::<ImDrawVert>() as c_int,
                                    offset_of(&ZERO_VERTEX.pos as *const _ as usize));
            gl::VertexAttribPointer(a_texture_uv as c_uint,
                                    2,
                                    gl::FLOAT,
                                    gl::FALSE,
                                    mem::size_of::<ImDrawVert>() as c_int,
                                    offset_of(&ZERO_VERTEX.uv as *const _ as usize));
            gl::VertexAttribPointer(a_color as c_uint,
                                    4,
                                    gl::UNSIGNED_BYTE,
                                    gl::TRUE,
                                    mem::size_of::<ImDrawVert>() as c_int,
                                    offset_of(&ZERO_VERTEX.col as *const _ as usize));

            gl::EnableVertexAttribArray(a_position as c_uint);
            gl::EnableVertexAttribArray(a_texture_uv as c_uint);
            gl::EnableVertexAttribArray(a_color as c_uint);

            Renderer {
                standard_font: standard_font,
                label_font: label_font,
                texture: texture,
                vertex_shader: vertex_shader,
                fragment_shader: fragment_shader,
                program: program,
                u_window_size: u_window_size,
                u_texture: u_texture,
                a_position: a_position,
                a_texture_uv: a_texture_uv,
                a_color: a_color,
                vbo: vbo,
            }
        }
    }

    fn ok_cancel_button(&self, exit_code: &mut Option<c_int>) {
        unsafe {
            let button_size = button_size();
            if imgui::igButton(b"OK\0" as *const c_uchar as *const c_char, button_size) {
                *exit_code = Some(0)
            }
            if imgui::igButton(b"Cancel\0" as *const c_uchar as *const c_char, button_size) {
                *exit_code = Some(1)
            }
        }
    }

    fn render_file_dialog(&self, subdialog: &mut FileDialog, exit_code: &mut Option<c_int>) {
        unsafe {
            imgui::igPushItemWidth(button_size().x);
            if imgui::igListBox(b"\0" as *const c_uchar as *const c_char,
                                &mut subdialog.entries.index,
                                subdialog.entries.entries.as_mut_ptr(),
                                subdialog.entries.entries.len() as c_int,
                                LIST_HEIGHT) {
                if subdialog.path.parent().is_some() && subdialog.entries.index == 0 {
                    subdialog.path = subdialog.path.parent().unwrap().to_owned();
                    subdialog.entries = FileDialogEntries::new(&subdialog.path)
                } else {
                    let (selected_path, file_type) = subdialog.selected_path();
                    match file_type {
                        SelectedFileType::File => *exit_code = Some(0),
                        SelectedFileType::Directory => {
                            subdialog.path = selected_path;
                            subdialog.entries = FileDialogEntries::new(&subdialog.path)
                        }
                    }
                }
            }
            igPopItemWidth();
            self.ok_cancel_button(exit_code);
            if *exit_code == Some(0) {
                println!("{}", subdialog.selected_path().0.display());
            }
        }
    }

    fn render_input_dialog(&self, subdialog: &mut InputDialog, exit_code: &mut Option<c_int>) {
        unsafe {
            imgui::igText(CString::new(subdialog.text.clone()).unwrap().as_ptr());
            imgui::igPushItemWidth(button_size().x);
            let data_c_string = subdialog.data.as_mut_ptr() as *mut c_uchar as *mut c_char;
            if imgui::igInputText(b"\0" as *const c_uchar as *const c_char,
                                  data_c_string,
                                  subdialog.data.len(),
                                  imgui::ImGuiInputTextFlags_EnterReturnsTrue,
                                  None,
                                  ptr::null_mut()) {
                *exit_code = Some(0)
            }
            igPopItemWidth();
            self.ok_cancel_button(exit_code);
            if *exit_code == Some(0) {
                let length = subdialog.data
                                      .iter()
                                      .position(|&x| x == 0)
                                      .unwrap_or(subdialog.data.len());
                io::stdout().write_all(&subdialog.data[..length]).unwrap();
                println!("");
            }
        }
    }

    fn render_menu_dialog(&self, subdialog: &mut MenuDialog, exit_code: &mut Option<c_int>) {
        unsafe {
            for item in &subdialog.items {
                if imgui::igSelectable(CString::new(item.tag.clone()).unwrap().as_ptr(),
                                       false,
                                       ImGuiSelectableFlags::empty(),
                                       ZERO_SIZE) {
                    println!("{}", item.tag);
                    *exit_code = Some(0)
                }

                imgui::igPushFont(self.label_font);
                imgui::igTextColored(LABEL_COLOR,
                                     CString::new(item.item.clone()).unwrap().as_ptr());
                imgui::igPopFont();
            }
        }
    }

    fn render(&self, window: &Window, dialog: &mut Dialog) -> Option<c_int> {
        let mut exit_code = None;
        unsafe {
            let (width, height) = window.size();
            gl::Viewport(0, 0, width as c_int, height as c_int);
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            imgui::igNewFrame();
            imgui::igSetNextWindowPosCenter(ImGuiSetCond::empty());
            imgui::igBegin(b"imdialog\0" as *const c_uchar as *const c_char,
                           &mut true,
                           imgui::ImGuiWindowFlags_NoTitleBar | imgui::ImGuiWindowFlags_NoResize);
            if !imgui::igIsAnyItemHovered() && !imgui::igIsAnyItemActive() {
                imgui::igSetKeyboardFocusHere(0)
            }

            match dialog.subdialog {
                Subdialog::File(ref mut subdialog) => {
                    self.render_file_dialog(subdialog, &mut exit_code)
                }
                Subdialog::Input(ref mut subdialog) => {
                    self.render_input_dialog(subdialog, &mut exit_code)
                }
                Subdialog::Menu(ref mut subdialog) => {
                    self.render_menu_dialog(subdialog, &mut exit_code)
                }
            }

            imgui::igEnd();

            RENDERER = self;
            imgui::igRender();
        }
        exit_code
    }

    fn render_draw_lists(&self, draw_data: &ImDrawData) {
        unsafe {
            gl::UseProgram(self.program);
            gl::Enable(gl::BLEND);
            gl::Enable(gl::SCISSOR_TEST);
            gl::Disable(gl::DEPTH_TEST);
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, self.texture);
            gl::Uniform2f(self.u_window_size, FRAMEBUFFER_WIDTH as f32, FRAMEBUFFER_HEIGHT as f32);
            gl::Uniform1i(self.u_texture, 0);

            let gl_buffer_type = if mem::size_of::<ImDrawIdx>() == 2 {
                gl::UNSIGNED_SHORT
            } else {
                gl::UNSIGNED_INT
            };

            for draw_list_index in 0..draw_data.cmd_lists_count {
                let draw_list = *draw_data.cmd_lists.offset(draw_list_index as isize);
                gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
                let vertex_buffer_size = imgui::ImDrawList_GetVertexBufferSize(draw_list) *
                    (mem::size_of::<ImDrawVert>() as c_int);
                gl::BufferData(gl::ARRAY_BUFFER,
                               vertex_buffer_size as intptr_t,
                               imgui::ImDrawList_GetVertexPtr(draw_list, 0) as *const c_void,
                               gl::DYNAMIC_DRAW);

                let mut index_offset = 0;
                for draw_command_index in 0..imgui::ImDrawList_GetCmdSize(draw_list) {
                    let draw_command = imgui::ImDrawList_GetCmdPtr(draw_list, draw_command_index);
                    let index_ptr = imgui::ImDrawList_GetIndexPtr(draw_list, 0);
                    let index_size = (*draw_command).elem_count;
                    let clip_rect = (*draw_command).clip_rect;
                    gl::Scissor(clip_rect.x as c_int,
                                ((FRAMEBUFFER_HEIGHT as f32) - clip_rect.w) as c_int,
                                (clip_rect.z - clip_rect.x) as c_int,
                                (clip_rect.w - clip_rect.y) as c_int);
                    gl::DrawElements(gl::TRIANGLES,
                                     index_size as c_int,
                                     gl_buffer_type,
                                     index_ptr.offset(index_offset) as *const c_void);
                    index_offset += index_size as isize
                }
            }
        }
    }
}

extern "C" fn render_draw_lists(draw_data: *mut ImDrawData) {
    unsafe {
        let draw_data: &ImDrawData = mem::transmute::<*mut ImDrawData, &ImDrawData>(draw_data);
        (mem::transmute::<*const Renderer, &Renderer>(RENDERER)).render_draw_lists(draw_data)
    }
}

fn set_mod_state(sdl: &Sdl) {
    unsafe {
        let io = imgui::igGetIO();
        let mod_state = sdl.keyboard().mod_state();
        (*io).key_shift = mod_state.intersects(keyboard::LSHIFTMOD | keyboard::RSHIFTMOD);
        (*io).key_ctrl = mod_state.intersects(keyboard::LCTRLMOD | keyboard::RCTRLMOD);
        (*io).key_alt = mod_state.intersects(keyboard::LALTMOD | keyboard::RALTMOD);
        (*io).key_super = mod_state.intersects(keyboard::LGUIMOD | keyboard::RGUIMOD);
    }
}

fn init_keys() {
    unsafe {
        let io = imgui::igGetIO();
        for (index, scancode) in SCANCODES.iter().enumerate() {
            (*io).key_map[index] = scancode.to_i32().unwrap()
        }
    }
}

fn init_texture() -> c_uint {
    unsafe {
        let io = imgui::igGetIO();
        let mut pixels = ptr::null_mut();
        let (mut width, mut height, mut bpp) = (0, 0, 0);
        imgui::ImFontAtlas_GetTexDataAsRGBA32((*io).fonts,
                                              &mut pixels,
                                              &mut width,
                                              &mut height,
                                              &mut bpp);

        let mut texture = 0;
        gl::GenTextures(1, &mut texture);
        gl::BindTexture(gl::TEXTURE_2D, texture);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as c_int);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as c_int);
        gl::TexImage2D(gl::TEXTURE_2D, 0,
                       gl::RGBA as c_int,
                       width, height,
                       0,
                       gl::RGBA,
                       gl::UNSIGNED_BYTE,
                       pixels as *const c_void);
        texture
    }
}

#[cfg(not(target_os="linux"))]
fn shutdown() {}

#[cfg(target_os="linux")]
fn shutdown() {
    if libc::isatty(0) {
        ioctl::kdskbmute(0, 0);
        ioctl::kdskbmode(0, K_XLATE);
    }
}

pub fn main() {
    let base_directories = BaseDirectories::with_prefix(PathBuf::from("imdialog/")).unwrap();
    let mut dialog = Dialog::new();

    let sdl = sdl2::init().unwrap();
    let video = sdl.video().unwrap();
    let window = video.window("imdialog", FRAMEBUFFER_WIDTH, FRAMEBUFFER_HEIGHT)
                      .position_centered()
                      .opengl()
                      .build()
                      .unwrap();

    let context = window.gl_create_context().unwrap();
    window.gl_make_current(&context).unwrap();
    gl::load_with(|name| video.gl_get_proc_address(name) as *const c_void);

    let renderer = Renderer::new(&base_directories);
   
    unsafe {
        let io = imgui::igGetIO();
        let (width, height) = window.size();
        (*io).display_size.x = width as f32;
        (*io).display_size.y = height as f32;
        (*io).render_draw_lists_fn = Some(render_draw_lists);
    }

    let mut events = sdl.event_pump().unwrap();
    let mut exit_code = 0;
    let mut event_queue = vec![];
    loop {
        if let Some(code) = renderer.render(&window, &mut dialog) {
            exit_code = code;
            break
        }

        if event_queue.is_empty() {
            event_queue.push(events.wait_event());
        }
        while let Some(event) = events.poll_event() {
            event_queue.push(event)
        }

        match event_queue.remove(0) {
            Event::Quit { .. } => break,
            Event::KeyDown { scancode: Some(scancode), .. } => {
                unsafe {
                    let io = imgui::igGetIO();
                    if let Some(scancode) = scancode.to_u8() {
                        (*io).keys_down[scancode as usize] = true
                    }
                    set_mod_state(&sdl);
                    if scancode == Scancode::Escape {
                        break
                    }
                }
            }
            Event::KeyUp { scancode: Some(scancode), .. } => {
                unsafe {
                    let io = imgui::igGetIO();
                    if let Some(scancode) = scancode.to_u8() {
                        (*io).keys_down[scancode as usize] = false
                    }
                    set_mod_state(&sdl);
                }
            }
            Event::TextInput { text, .. } => {
                unsafe {
                    if let Ok(text) = CString::new(text) {
                        imgui::ImGuiIO_AddInputCharactersUTF8(text.as_ptr())
                    }
                }
            }
            _ => {}
        }

        unsafe {
            let io = imgui::igGetIO();
            let (mouse_state, mouse_x, mouse_y) = sdl.mouse().mouse_state();
            (*io).mouse_pos.x = mouse_x as f32;
            (*io).mouse_pos.y = mouse_y as f32;
            (*io).mouse_down[0] = mouse_state.left();
            (*io).mouse_down[1] = mouse_state.right();
            (*io).mouse_down[2] = mouse_state.middle();
        }

        if let Some(code) = renderer.render(&window, &mut dialog) {
            exit_code = code;
            break
        }

        window.gl_swap_window();
    }

    shutdown();
    process::exit(exit_code)
}

extern {
    fn igPopItemWidth();
}

#[cfg(windows)]
#[link_args = "-limm32"]
extern {
    // link hack
}

