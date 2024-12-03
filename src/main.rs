extern crate ffmpeg_next as ffmpeg;

use std::sync::mpsc;
use sdl2::{pixels::Color, render::Canvas, video::Window, VideoSubsystem};
use ffmpeg_next::frame::Video as AVFrame;

mod control;
use crate::control::player::PlayerControl;

// 定义固定窗口大小
const WINDOW_WIDTH: u32 = 800;
const WINDOW_HEIGHT: u32 = 600;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize SDL things
    let sdl_context = sdl2::init().unwrap();
    let video_subsystem = sdl_context.video().unwrap();

    let window = create_window(&video_subsystem, WINDOW_WIDTH, WINDOW_HEIGHT);
    let mut canvas = create_canvas(window);

    let texture_creator = canvas.texture_creator();
  

    // 创建通道用于传输视频帧数据
    let (tx, rx) = mpsc::channel::<AVFrame>();

    PlayerControl::start(
        "/Users/chinaxxren/Desktop/a.mp4".into(),
        {
            move |video| {
                let width = video.width();
                let height = video.height();
                
                if width == 0 || height == 0 {
                    println!("Error: Invalid frame dimensions: {}x{}", width, height);
                    return;
                }
                
                println!("Frame received: {}x{}, format: {:?}", width, height, video.format());
                
                // 确保宽度和高度是2的倍数
                if width % 2 != 0 || height % 2 != 0 {
                    println!("Error: Frame dimensions not multiple of 2: {}x{}", width, height);
                    return;
                }
                
                // 检查帧格式
                if frame.format() != ffmpeg::format::Pixel::YUV420P {
                    println!("Error: Unexpected frame format: {:?}", video.format());
                    return;
                }
                
               
            }
        },
        {
            move |playing| {
                println!("Playing state changed: {}", playing);
            }
        },
    )
    .unwrap();


    println!("############################=>Exiting...");
    Ok(())
}

fn create_window(video_subsystem: &VideoSubsystem, width: u32, height: u32) -> Window { 
    video_subsystem
        .window("Video Player", width, height)
        .position_centered()
        .resizable() // 允许调整窗口大小
        .opengl()
        .build()
        .unwrap()
}

fn create_canvas(window: Window) -> Canvas<Window> {
    let mut canvas = window
        .into_canvas()
        .present_vsync() // 启用垂直同步
        .build()
        .unwrap();

    canvas.set_draw_color(Color::RGB(0, 0, 0));
    canvas.clear();
    canvas.present();

    canvas
}