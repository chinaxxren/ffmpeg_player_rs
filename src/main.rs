mod core;

use crate::core::decode::Decoder;
use crate::core::Url;
use crate::core::init;
use ndarray;

fn main() {
    init::init().unwrap();

    let source =
        "http://commondatastorage.googleapis.com/gtv-videos-bucket/sample/BigBuckBunny.mp4"
            .parse::<Url>()
            .unwrap();
    let mut decoder = Decoder::new(source).expect("failed to create decoder");

    for frame in decoder.decode_iter() {
        if let Ok((_, frame)) = frame {
            let rgb = frame.slice(ndarray::s![0, 0, ..]).to_slice().unwrap();
            println!("pixel at 0, 0: {}, {}, {}", rgb[0], rgb[1], rgb[2],);
        } else {
            break;
        }
    }
}