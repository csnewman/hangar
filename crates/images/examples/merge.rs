//! An EROFS image of layer tar archives, top first, as a pull makes one:
//!
//!   cargo run -p hangar-images --example merge -- OUT LAYER...

use std::fs::File;
use std::io::Read;
use std::path::Path;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((out, layers)) = args.split_first() else {
        eprintln!("usage: merge OUT LAYER...");
        std::process::exit(2);
    };
    let mut layers = layers.iter();
    hangar_images::build_erofs(Path::new(out), || {
        layers
            .next()
            .map(|l| File::open(l).map(|f| Box::new(f) as Box<dyn Read>))
            .transpose()
    })
}
