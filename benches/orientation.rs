#![feature(test)]

extern crate oxipng;
extern crate test;

use std::path::PathBuf;

use oxipng::{internal_tests::*, *};
use test::Bencher;

#[bench]
fn orientation_2(b: &mut Bencher) {
    let input = test::black_box(PathBuf::from("tests/files/orientation_2.png"));
    let options = Options {
        strip: StripChunks::Safe,
        ..Default::default()
    };
    let png = PngData::new(&input, &options).unwrap();

    b.iter(|| interlace::restructured(&png.raw, None, true));
}

#[bench]
fn orientation_3(b: &mut Bencher) {
    let input = test::black_box(PathBuf::from("tests/files/orientation_3.png"));
    let options = Options {
        strip: StripChunks::Safe,
        ..Default::default()
    };
    let png = PngData::new(&input, &options).unwrap();

    b.iter(|| interlace::restructured(&png.raw, None, true));
}

#[bench]
fn orientation_4(b: &mut Bencher) {
    let input = test::black_box(PathBuf::from("tests/files/orientation_4.png"));
    let options = Options {
        strip: StripChunks::Safe,
        ..Default::default()
    };
    let png = PngData::new(&input, &options).unwrap();

    b.iter(|| interlace::restructured(&png.raw, None, true));
}

#[bench]
fn orientation_5(b: &mut Bencher) {
    let input = test::black_box(PathBuf::from("tests/files/orientation_5.png"));
    let options = Options {
        strip: StripChunks::Safe,
        ..Default::default()
    };
    let png = PngData::new(&input, &options).unwrap();

    b.iter(|| interlace::restructured(&png.raw, None, true));
}

#[bench]
fn orientation_6(b: &mut Bencher) {
    let input = test::black_box(PathBuf::from("tests/files/orientation_6.png"));
    let options = Options {
        strip: StripChunks::Safe,
        ..Default::default()
    };
    let png = PngData::new(&input, &options).unwrap();

    b.iter(|| interlace::restructured(&png.raw, None, true));
}

#[bench]
fn orientation_7(b: &mut Bencher) {
    let input = test::black_box(PathBuf::from("tests/files/orientation_7.png"));
    let options = Options {
        strip: StripChunks::Safe,
        ..Default::default()
    };
    let png = PngData::new(&input, &options).unwrap();

    b.iter(|| interlace::restructured(&png.raw, None, true));
}

#[bench]
fn orientation_8(b: &mut Bencher) {
    let input = test::black_box(PathBuf::from("tests/files/orientation_8.png"));
    let options = Options {
        strip: StripChunks::Safe,
        ..Default::default()
    };
    let png = PngData::new(&input, &options).unwrap();

    b.iter(|| interlace::restructured(&png.raw, None, true));
}
