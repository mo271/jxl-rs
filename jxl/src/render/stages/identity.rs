// Copyright (c) the JPEG XL Project Authors. All rights reserved.
//
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

use crate::{
    headers::CustomTransformData,
    render::{RenderPipelineInOutStage, RenderPipelineStage},
};

pub struct Identity {
    channel: usize,
}

impl Identity {
    pub fn new(channel: usize) -> Identity {
        Identity { channel }
    }
}

impl std::fmt::Display for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Identity transform of channel {}", self.channel)
    }
}

impl RenderPipelineStage for Identity {
    type Type = RenderPipelineInOutStage<f32, f32, 0, 0, 0, 0>;

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
                output[i][j] = input[i][j];
            }
        }
    }
    fn new_size(&self, current_size: (usize, usize)) -> (usize, usize) {
        current_size
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
    fn test_identity() -> Result<()> {
        let mut input = Image::new((2, 2))?;
        let val: f32 = 7.7;
        input.as_rect_mut().row(0).copy_from_slice(&[val, val]);
        let stage = Identity::new(0);
        let output: Vec<Image<f32>> = make_and_run_simple_pipeline(stage, &[input], (2, 2), 512)?.1;
        assert_eq!(output[0].as_rect().row(0)[1], val);
        assert_eq!(output[0].as_rect().row(1)[1], 0.0);
        Ok(())
    }
}
