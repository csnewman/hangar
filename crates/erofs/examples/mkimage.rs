//! An image of a directory, as `hangar erofs -o IMAGE DIR` makes one:
//!
//!   cargo run -p hangar-erofs --example mkimage -- DIR IMAGE

use std::fs::File;
use std::path::Path;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, dir, image] = args.as_slice() else {
        eprintln!("usage: mkimage DIR IMAGE");
        std::process::exit(2);
    };
    let mut w = hangar_erofs::Writer::new(File::create(image)?);
    w.add_dir(Path::new(dir))?;
    w.close()?;
    let size = w.size();
    w.into_inner().set_len(size)
}
