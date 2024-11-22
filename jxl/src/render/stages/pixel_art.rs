// Copyright (c) the JPEG XL Project Authors. All rights reserved.
//
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

use crate::{
    render::{RenderPipelineInOutStage, RenderPipelineStage},
};

pub struct PixelArt {
    channel: usize,
}

impl PixelArt {
    pub fn new(channel: usize) -> PixelArt {
        PixelArt { channel }
    }
}

impl std::fmt::Display for PixelArt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Scaling by 2x of channel {}", self.channel)
    }
}

impl RenderPipelineStage for PixelArt {
    type Type = RenderPipelineInOutStage<f32, f32, 0, 0, 1, 1>;

    fn uses_channel(&self, c: usize) -> bool {
        c == self.channel
    }
    fn process_row_chunk(
        &mut self,
        _position: (usize, usize),
        _xsize: usize,
        row: &mut [(&[&[f32]], &mut [&mut [f32]])],
    ) {
        let (input, output) = &mut row[0];
        for i in 0..input.len() {
            for j in 0..input[0].len() {
                for di in 0..2 {
                    for dj in 0..2 {
                        output[2 * i + di][2 * j + dj] = input[i][j];
                    }
                }
            }
        }
    }
    fn new_size(&self, current_size: (usize, usize)) -> (usize, usize) {
        (2 * current_size.0, 2 * current_size.1)
    }

    fn original_data_origin(&self) -> (usize, usize) {
        (0, 0)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{error::Result, image::Image, render::test::make_and_run_simple_pipeline};
    use test_log::test;

    #[test]
    fn test_pixel_art() -> Result<()> {
        let mut input = Image::new((2, 2))?;
        let val: f32 = 7.7;
        input.as_rect_mut().row(0).copy_from_slice(&[val, val]);
        let stage = PixelArt::new(0);
        let output: Vec<Image<f32>> = make_and_run_simple_pipeline(stage, &[input], (2, 2), 512).unwrap().1;
        assert_eq!(output[0].as_rect().row(0)[1], val);
        assert_eq!(output[0].as_rect().row(1)[1], 0.0);
        Ok(())
    }
}
