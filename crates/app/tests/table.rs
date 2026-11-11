//! CSV and TSV read as a table: quoted fields with separators, doubled
//! quotes and line breaks in them, Windows line ends, and each column's
//! width.

use hangar_app::table::{rows, separator, widths};

#[test]
fn rows_are_read_as_rfc_4180_has_them() {
    let text = "name,note\r\nAda,\"likes, commas\"\nBob,\"says \"\"hi\"\"\"\nCy,\"two\nlines\"\n";
    assert_eq!(
        rows(text, ','),
        [
            vec!["name", "note"],
            vec!["Ada", "likes, commas"],
            vec!["Bob", "says \"hi\""],
            vec!["Cy", "two\nlines"],
        ]
    );
    assert_eq!(rows("a\tb\n1\t2", '\t'), [vec!["a", "b"], vec!["1", "2"]]);
    assert_eq!(separator("/w/data.TSV"), Some('\t'));
    assert_eq!(separator("/w/data.csv"), Some(','));
    assert_eq!(separator("/w/data.txt"), None);
    assert_eq!(widths(&rows("a,bbbb\nccccc,d\n", ',')), [5, 4]);
}
