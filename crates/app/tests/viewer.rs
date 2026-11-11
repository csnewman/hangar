//! How files are shown other than as text: by their names, a picture as
//! one, a PDF, sound or video by this computer's app, everything else as
//! text until it is found not to be; and bytes sixteen a row.

use hangar_app::viewer::{View, hex_row, opens_as, ways};

#[test]
fn files_open_by_their_names() {
    assert_eq!(opens_as("/w/logo.PNG"), View::Image);
    assert_eq!(opens_as("/w/icon.svg"), View::Text, "an SVG is text");
    assert_eq!(opens_as("/w/talk.pdf"), View::External);
    assert_eq!(opens_as("/w/clip.mov"), View::External);
    assert_eq!(opens_as("/w/main.rs"), View::Text);
    assert_eq!(
        ways("/w/icon.svg"),
        [View::Text, View::Image, View::External, View::Hex]
    );
    assert_eq!(ways("/w/main.rs"), [View::Text, View::External, View::Hex]);
}

#[test]
fn bytes_are_shown_sixteen_a_row() {
    let row = hex_row(0x20, b"PNG\x00\x01hello world");
    assert_eq!(
        row,
        "00000020  50 4e 47 00 01 68 65 6c  6c 6f 20 77 6f 72 6c 64  PNG..hello world"
    );
    assert!(hex_row(0, b"ab").starts_with("00000000  61 62    "));
}

/// An HTML file can open in the browser, from the preview server under its
/// secret, each part of its path escaped.
#[test]
fn html_opens_in_the_browser() {
    use hangar_app::viewer::preview_url;
    assert_eq!(
        ways("/w/index.html"),
        [View::Text, View::External, View::Hex, View::Browser]
    );
    assert_eq!(
        preview_url(
            "http://40123-envabc123.hangar.localhost:8080/",
            "s3cr3t",
            "/home/dev/my site/index.html"
        ),
        "http://40123-envabc123.hangar.localhost:8080/s3cr3t/home/dev/my%20site/index.html"
    );
}
