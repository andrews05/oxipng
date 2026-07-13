use std::{
    fs::remove_file,
    path::{Path, PathBuf},
};

use oxipng::{internal_tests::*, *};

fn get_opts(input: &Path) -> (OutFile, oxipng::Options) {
    let options = oxipng::Options {
        force: true,
        strip: StripChunks::Safe,
        filters: indexset! {FilterStrategy::NONE},
        ..Default::default()
    };
    (OutFile::from_path(input.with_extension("out.png")), options)
}

fn test_it_converts(input: &str, orientation: u8) {
    let input = PathBuf::from(input);
    let (output, opts) = get_opts(&input);
    let png = PngData::new(&input, &opts).unwrap();
    assert_eq!(png.raw.ihdr.orientation as u8, orientation);

    match oxipng::optimize(&InFile::Path(input), &output, &opts) {
        Ok(_) => (),
        Err(x) => panic!("{}", x),
    }
    let output = output.path().unwrap();
    assert!(output.exists());

    let png = match PngData::new(output, &opts) {
        Ok(x) => x,
        Err(x) => {
            remove_file(output).ok();
            panic!("{}", x)
        }
    };

    assert_eq!(png.raw.ihdr.orientation as u8, 1);

    remove_file(output).ok();
}

#[test]
fn orientation_1() {
    test_it_converts("tests/files/orientation_1.png", 1);
}

#[test]
fn orientation_2() {
    test_it_converts("tests/files/orientation_2.png", 2);
}

#[test]
fn orientation_3() {
    test_it_converts("tests/files/orientation_3.png", 3);
}

#[test]
fn orientation_4() {
    test_it_converts("tests/files/orientation_4.png", 4);
}

#[test]
fn orientation_5() {
    test_it_converts("tests/files/orientation_5.png", 5);
}

#[test]
fn orientation_6() {
    test_it_converts("tests/files/orientation_6.png", 6);
}

#[test]
fn orientation_7() {
    test_it_converts("tests/files/orientation_7.png", 7);
}

#[test]
fn orientation_8() {
    test_it_converts("tests/files/orientation_8.png", 8);
}
