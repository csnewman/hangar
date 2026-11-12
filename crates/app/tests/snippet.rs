use hangar_app::snippet::expand;

#[test]
fn snippets_read_as_their_text_with_their_first_stop() {
    assert_eq!(expand("println!($0)"), ("println!()".into(), Some(9..9)));
    assert_eq!(
        expand("fn ${1:name}(${2:args}) {\n    $0\n}"),
        ("fn name(args) {\n    \n}".into(), Some(3..7))
    );
    assert_eq!(expand("${2:b} ${1:a}"), ("b a".into(), Some(2..3)));
    assert_eq!(
        expand("let ${1|mut,const|} x"),
        ("let mut x".into(), Some(4..7))
    );
    assert_eq!(
        expand("${1:outer ${2:inner}} end"),
        ("outer inner end".into(), Some(0..11))
    );
    assert_eq!(
        expand("cost \\$5 \\} $TM_FILENAME${VAR:dflt}"),
        ("cost $5 } dflt".into(), None)
    );
    assert_eq!(expand("plain"), ("plain".into(), None));
    assert_eq!(expand("a $"), ("a $".into(), None));
}
