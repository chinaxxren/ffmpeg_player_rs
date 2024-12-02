
extern crate ffmpeg_next as ffmpeg;

use ffmpeg::format::Pixel;

mod control;

use crate::control::player::PlayerControl;

fn main() {

    let mut player = PlayerControl::start(
        "http://commondatastorage.googleapis.com/gtv-videos-bucket/sample/TearsOfSteel.mp4".into(),
        {

            move |new_frame| {

                // let rebuild_rescaler =
                //     to_rgba_rescaler.as_ref().map_or(true, |existing_rescaler| {
                //         existing_rescaler.input().format != new_frame.format()
                //     });

                // if rebuild_rescaler {
                //     to_rgba_rescaler = Some(rgba_rescaler_for_frame(new_frame));
                // }

                // let rescaler = to_rgba_rescaler.as_mut().unwrap();

                // let mut rgb_frame = ffmpeg::util::frame::Video::empty();
                // rescaler.run(&new_frame, &mut rgb_frame).unwrap();

                // let pixel_buffer = video_frame_to_pixel_buffer(&rgb_frame);
                // app_weak.upgrade_in_event_loop(|app| {
                        //app.set_video_frame(slint::Image::from_rgb8(pixel_buffer))
                    // })
                    // .unwrap();
            }
        },
        {
            // let app_weak = app.as_weak();

            move |playing| {
                // app_weak.upgrade_in_event_loop(move |app| app.set_playing(playing)).unwrap();
            }
        },
    )
    .unwrap();

    // app.on_toggle_pause_play(move || {
    //     player.toggle_pause_playing();
    // });

    // app.run().unwrap();
}

// Work around https://github.com/zmwangx/rust-ffmpeg/issues/102
struct Rescaler(ffmpeg::software::scaling::Context);
unsafe impl std::marker::Send for Rescaler {}

fn rgba_rescaler_for_frame(frame: &ffmpeg_next::util::frame::Video) -> Rescaler {
    Rescaler(
        ffmpeg_next::software::scaling::Context::get(
            frame.format(),
            frame.width(),
            frame.height(),
            Pixel::RGB24,
            frame.width(),
            frame.height(),
            ffmpeg_next::software::scaling::Flags::BILINEAR,
        )
        .unwrap(),
    )
}