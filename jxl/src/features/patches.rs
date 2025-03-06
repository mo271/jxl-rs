// Copyright (c) the JPEG XL Project Authors. All rights reserved.
//
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

// TODO(firsching): remove once we use this!
#![allow(dead_code)]

use num_derive::FromPrimitive;
use num_traits::FromPrimitive;

use crate::{
    bit_reader::BitReader, entropy_coding::decode::Histograms, error::{Error, Result}, frame::DecoderState, render::{RenderPipelineInPlaceStage, RenderPipelineStage}, util::{tracing_wrappers::*, NewWithCapacity}
};

pub struct PatchesStage {
    patches: PatchesDictionary,
}

impl std::fmt::Display for PatchesStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "patches")
    }
}

impl RenderPipelineStage for PatchesStage {
    type Type = RenderPipelineInPlaceStage<f32>;

    fn uses_channel(&self, c: usize) -> bool {
        c < 3
    }

    fn process_row_chunk(
        &mut self,
        position: (usize, usize),
        xsize: usize,
        row: &mut [&mut [f32]],
    ) {
        self.patches.add_one_row(row, position, xsize);
    }
}

// Context numbers as specified in Section C.4.5, Listing C.2:
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[repr(usize)]
pub enum PatchContext {
    NumRefPatch = 0,
    ReferenceFrame = 1,
    PatchSize = 2,
    PatchReferencePosition = 3,
    PatchPosition = 4,
    PatchBlendMode = 5,
    PatchOffset = 6,
    PatchCount = 7,
    PatchAlphaChannel = 8,
    PatchClamp = 9,
}

impl PatchContext {
    const NUM: usize = 10;
}

/// Blend modes
#[derive(Debug, PartialEq, Eq, Clone, Copy, FromPrimitive)]
#[repr(u8)]
pub enum PatchBlendMode {
    // The new values are the old ones. Useful to skip some channels.
    None = 0,
    // The new values (in the crop) replace the old ones: sample = new
    Replace = 1,
    // The new values (in the crop) get added to the old ones: sample = old + new
    Add = 2,
    // The new values (in the crop) get multiplied by the old ones:
    // sample = old * new
    // This blend mode is only supported if BlendColorSpace is kEncoded. The
    // range of the new value matters for multiplication purposes, and its
    // nominal range of 0..1 is computed the same way as this is done for the
    // alpha values in kBlend and kAlphaWeightedAdd.
    Mul = 3,
    // The new values (in the crop) replace the old ones if alpha>0:
    // For first alpha channel:
    // alpha = old + new * (1 - old)
    // For other channels if !alpha_associated:
    // sample = ((1 - new_alpha) * old * old_alpha + new_alpha * new) / alpha
    // For other channels if alpha_associated:
    // sample = (1 - new_alpha) * old + new
    // The alpha formula applies to the alpha used for the division in the other
    // channels formula, and applies to the alpha channel itself if its
    // blend_channel value matches itself.
    // If using kBlendAbove, new is the patch and old is the original image; if
    // using kBlendBelow, the meaning is inverted.
    BlendAbove = 4,
    BlendBelow = 5,
    // The new values (in the crop) are added to the old ones if alpha>0:
    // For first alpha channel: sample = sample = old + new * (1 - old)
    // For other channels: sample = old + alpha * new
    AlphaWeightedAddAbove = 6,
    AlphaWeightedAddBelow = 7,
}

impl PatchBlendMode {
    pub const NUM_BLEND_MODES: u8 = 8;

    pub fn uses_alpha(self) -> bool {
        matches!(
            self,
            Self::BlendAbove
                | Self::BlendBelow
                | Self::AlphaWeightedAddAbove
                | Self::AlphaWeightedAddBelow
        )
    }

    pub fn uses_clamp(self) -> bool {
        self.uses_alpha() || self == Self::Mul
    }
}

#[derive(Debug, Clone, Copy)]
struct PatchBlending {
    mode: PatchBlendMode,
    alpha_channel: usize,
    clamp: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct PatchReferencePosition {
    // Not using `ref` like in the spec here, because it is a keyword.
    reference: usize,
    x0: usize,
    y0: usize,
    xsize: usize,
    ysize: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PatchPosition {
    x: usize,
    y: usize,
    ref_pos_idx: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct PatchTreeNode {
    left_child: isize,
    right_child: isize,
    y_center: usize,
    start: usize,
    num: usize,
}

#[derive(Debug, Default)]
pub struct PatchesDictionary {
    pub positions: Vec<PatchPosition>,
    ref_positions: Vec<PatchReferencePosition>,
    blendings: Vec<PatchBlending>,
    blendings_stride: usize,
    patch_tree: Vec<PatchTreeNode>,
    // Number of patches for each row.
    num_patches: Vec<usize>,
    sorted_patches_y0: Vec<(usize, usize)>,
    sorted_patches_y1: Vec<(usize, usize)>,
}

impl PatchesDictionary {

    fn compute_patch_tree(&mut self) {
        #[derive(Debug, Clone, Copy)]
        struct PatchInterval {
            idx: usize,
            y0: usize,
            y1: usize,
        }

        self.patch_tree.clear();
        self.num_patches.clear();
        self.sorted_patches_y0.clear();
        self.sorted_patches_y1.clear();

        if self.positions.is_empty() {
            return;
        }

        // Create a y-interval for each patch.
        let mut intervals: Vec<PatchInterval> = Vec::with_capacity(self.positions.len());
        for (i, pos) in self.positions.iter().enumerate() {
            intervals.push(PatchInterval {
                idx: i,
                y0: pos.y,
                y1: pos.y + self.ref_positions[pos.ref_pos_idx].ysize,
            });
        }

        let mut sort_by_y0 = |start: usize, end: usize| {
            intervals[start..end].sort_unstable_by_key(|i| i.y0);
        };
        let mut sort_by_y1 = |start: usize, end: usize| {
            intervals[start..end].sort_unstable_by_key(|i| i.y1);
        };

        // Count the number of patches for each row.
        sort_by_y1(0, intervals.len());
        self.num_patches.resize(intervals.last().map_or(0, |iv| iv.y1), 0); //Safe last()
        for iv in &intervals {
            for y in iv.y0..iv.y1 {
                self.num_patches[y] += 1;
            }
        }

        let mut root = PatchTreeNode::default();
        root.start = 0;
        root.num = intervals.len();
        self.patch_tree.push(root);

        let mut next = 0;
        while next < self.patch_tree.len() {
            let node = &mut self.patch_tree[next]; // Borrow mutably *before* accessing fields
            let start = node.start;
            let end = node.start + node.num;

            // Choose the y_center for this node to be the median of interval starts.
            sort_by_y0(start, end);
            let middle_idx = start + node.num / 2;
            node.y_center = intervals[middle_idx].y0;

            // Divide the intervals in [start, end) into three groups:
            let mut right_start = middle_idx;
            while right_start < end && intervals[right_start].y0 == node.y_center {
                right_start += 1;
            }

            sort_by_y1(start, right_start);
            let mut left_end = right_start;
            while left_end > start && intervals[left_end - 1].y1 > node.y_center {
                left_end -= 1;
            }

            // Fill in sorted_patches_y0_ and sorted_patches_y1_ for the current node.
            node.num = right_start - left_end;
            node.start = self.sorted_patches_y0.len();

            for i in (left_end..right_start).rev() {
                self.sorted_patches_y1
                    .push((intervals[i].y1, intervals[i].idx));
            }
            sort_by_y0(left_end, right_start);
            for i in left_end..right_start {
                self.sorted_patches_y0
                    .push((intervals[i].y0, intervals[i].idx));
            }

            // Create the left and right nodes (if not empty).
            // We modify left_child/right_child on the *original* node in patch_tree,
            // so we have to do the assignment *before* we push the new nodes.
            self.patch_tree[next].left_child = -1;
            self.patch_tree[next].right_child = -1;

            if left_end > start {
                let mut left = PatchTreeNode::default();
                left.start = start;
                left.num = left_end - left.start;
                self.patch_tree[next].left_child = self.patch_tree.len() as isize;
                self.patch_tree.push(left);
            }
            if right_start < end {
                let mut right = PatchTreeNode::default();
                right.start = right_start;
                right.num = end - right.start;
                self.patch_tree[next].right_child = self.patch_tree.len() as isize;
                self.patch_tree.push(right);
            }

            next += 1;
        }
    }

    #[instrument(level = "debug", skip(br), ret, err)]
    pub fn read(
        br: &mut BitReader,
        xsize: usize,
        ysize: usize,
        decoder_state: &DecoderState,
    ) -> Result<PatchesDictionary> {
        let num_extra_channels = decoder_state.extra_channel_info().len();
        let blendings_stride = num_extra_channels + 1;
        let patches_histograms = Histograms::decode(PatchContext::NUM, br, true)?;
        let mut patches_reader = patches_histograms.make_reader(br)?;
        let num_ref_patch = patches_reader.read(br, PatchContext::NumRefPatch as usize)? as usize;
        let num_pixels = xsize * ysize;
        let max_ref_patches = 1024 + num_pixels / 4;
        let max_patches = max_ref_patches * 4;
        let max_blending_infos = max_patches * 4;
        if num_ref_patch > max_ref_patches {
            return Err(Error::PatchesTooMany(
                "reference patches".to_string(),
                num_ref_patch,
                max_ref_patches,
            ));
        }
        let mut total_patches = 0;
        let mut next_size = 1;
        let mut positions: Vec<PatchPosition> = Vec::new();
        let mut blendings = Vec::new();
        let mut ref_positions = Vec::new_with_capacity(num_ref_patch)?;
        for _ in 0..num_ref_patch {
            let reference =
                patches_reader.read(br, PatchContext::ReferenceFrame as usize)? as usize;
            if reference >= DecoderState::MAX_STORED_FRAMES {
                return Err(Error::PatchesRefTooLarge(
                    reference,
                    DecoderState::MAX_STORED_FRAMES,
                ));
            }

            let x0 =
                patches_reader.read(br, PatchContext::PatchReferencePosition as usize)? as usize;
            let y0 =
                patches_reader.read(br, PatchContext::PatchReferencePosition as usize)? as usize;
            let ref_pos_xsize =
                patches_reader.read(br, PatchContext::PatchSize as usize)? as usize + 1;
            let ref_pos_ysize =
                patches_reader.read(br, PatchContext::PatchSize as usize)? as usize + 1;
            let reference_frame = decoder_state.reference_frame(reference);
            // TODO(firsching): make sure this check is correct in the presence of downsampled extra channels (also in libjxl).
            match reference_frame {
                None => return Err(Error::PatchesInvalidReference(reference)),
                Some(reference) => {
                    if !reference.saved_before_color_transform {
                        return Err(Error::PatchesPostColorTransform());
                    }
                    if x0 + ref_pos_xsize > reference.frame[0].size.0 {
                        return Err(Error::PatchesInvalidPosition(
                            "x".to_string(),
                            x0,
                            ref_pos_xsize,
                            reference.frame[0].size.0,
                        ));
                    }
                    if y0 + ref_pos_ysize > reference.frame[0].size.1 {
                        return Err(Error::PatchesInvalidPosition(
                            "y".to_string(),
                            y0,
                            ref_pos_ysize,
                            reference.frame[0].size.1,
                        ));
                    }
                }
            }

            let id_count = patches_reader.read(br, PatchContext::PatchCount as usize)? as usize + 1;
            if id_count > max_patches + 1 {
                return Err(Error::PatchesTooMany(
                    "patches".to_string(),
                    id_count,
                    max_patches,
                ));
            }
            total_patches += id_count;

            if total_patches > max_patches {
                return Err(Error::PatchesTooMany(
                    "patches".to_string(),
                    total_patches,
                    max_patches,
                ));
            }

            if next_size < total_patches {
                next_size *= 2;
                next_size = std::cmp::min(next_size, max_patches);
            }
            if next_size * blendings_stride > max_blending_infos {
                return Err(Error::PatchesTooMany(
                    "blending_info".to_string(),
                    total_patches,
                    max_patches,
                ));
            }
            positions.try_reserve(next_size.saturating_sub(positions.len()))?;
            blendings.try_reserve(
                (next_size * PatchBlendMode::NUM_BLEND_MODES as usize)
                    .saturating_sub(blendings.len()),
            )?;

            for i in 0..id_count {
                let mut pos = PatchPosition {
                    x: 0,
                    y: 0,
                    ref_pos_idx: ref_positions.len(),
                };
                if i == 0 {
                    // Read initial position
                    pos.x = patches_reader.read(br, PatchContext::PatchPosition as usize)? as usize;
                    pos.y = patches_reader.read(br, PatchContext::PatchPosition as usize)? as usize;
                } else {
                    // Read offsets and calculate new position
                    let delta_x =
                        patches_reader.read_signed(br, PatchContext::PatchOffset as usize)?;
                    if delta_x < 0 && (-delta_x as usize) > positions.last().unwrap().x {
                        return Err(Error::PatchesInvalidDelta(
                            "x".to_string(),
                            positions.last().unwrap().x,
                            delta_x,
                        ));
                    }
                    pos.x = (positions.last().unwrap().x as i32 + delta_x) as usize;

                    let delta_y =
                        patches_reader.read_signed(br, PatchContext::PatchOffset as usize)?;
                    if delta_y < 0 && (-delta_y as usize) > positions.last().unwrap().y {
                        return Err(Error::PatchesInvalidDelta(
                            "y".to_string(),
                            positions.last().unwrap().y,
                            delta_y,
                        ));
                    }
                    pos.y = (positions.last().unwrap().y as i32 + delta_y) as usize;
                }

                if pos.x + ref_pos_xsize > xsize {
                    return Err(Error::PatchesOutOfBounds(
                        "x".to_string(),
                        pos.x,
                        ref_pos_xsize,
                        xsize,
                    ));
                }
                if pos.y + ref_pos_ysize > ysize {
                    return Err(Error::PatchesOutOfBounds(
                        "y".to_string(),
                        pos.y,
                        ref_pos_ysize,
                        ysize,
                    ));
                }

                let mut alpha_channel = 0;
                let mut clamp = false;
                for _ in 0..blendings_stride {
                    let maybe_blend_mode =
                        patches_reader.read(br, PatchContext::PatchBlendMode as usize)? as u8;
                    let blend_mode = match PatchBlendMode::from_u8(maybe_blend_mode) {
                        None => {
                            return Err(Error::PatchesInvalidBlendMode(
                                maybe_blend_mode,
                                PatchBlendMode::NUM_BLEND_MODES,
                            ))
                        }
                        Some(blend_mode) => blend_mode,
                    };

                    if PatchBlendMode::uses_alpha(blend_mode) {
                        alpha_channel = patches_reader
                            .read(br, PatchContext::PatchAlphaChannel as usize)?
                            as usize;
                        if alpha_channel >= num_extra_channels {
                            return Err(Error::PatchesInvalidAlphaChannel(
                                alpha_channel,
                                num_extra_channels,
                            ));
                        }
                    }

                    if PatchBlendMode::uses_clamp(blend_mode) {
                        clamp = patches_reader.read(br, PatchContext::PatchClamp as usize)? != 0;
                    }
                    blendings.push(PatchBlending {
                        mode: blend_mode,
                        alpha_channel,
                        clamp,
                    });
                }
                positions.push(pos);
            }

            ref_positions.push(PatchReferencePosition {
                reference,
                x0,
                y0,
                xsize: ref_pos_xsize,
                ysize: ref_pos_ysize,
            })
        }

        let mut patches_dict =
        PatchesDictionary {
            positions,
            blendings,
            ref_positions,
            blendings_stride,
            num_patches: vec![],
            sorted_patches_y0: vec![],
            sorted_patches_y1: vec![],
            patch_tree: vec![],
        };
        patches_dict.compute_patch_tree();
        Ok(patches_dict)
    }

    pub fn get_patches_for_row(&self, y: usize) -> Vec<usize> {
        let mut result = vec![];
        if self.num_patches.len() <= y || self.num_patches[y] == 0 {
            return result;
        }

        result.reserve(self.num_patches[y]); //  Reserve space as in C++

        let mut tree_idx: isize = 0;
        loop {
            if tree_idx == -1 {
                break;
            }

            // Safe access using get() and unwrap_or().  No need for the assert.
            let node = self.patch_tree.get(tree_idx as usize).unwrap_or_else(|| {
                // TODO(firsching): Handle panic differently?
                panic!("Invalid tree_idx: {}", tree_idx);
            });


            if y <= node.y_center {
                for i in 0..node.num {
                    let p = self.sorted_patches_y0[node.start + i];
                    if y < p.0 {
                        break;
                    }
                    result.push(p.1);
                }
                tree_idx = if y < node.y_center {
                    node.left_child
                } else {
                    -1
                };
            } else {
                for i in 0..node.num {
                    let p = self.sorted_patches_y1[node.start + i];
                    if y >= p.0 {
                        break;
                    }
                    result.push(p.1);
                }
                tree_idx = node.right_child;
            }
        }

        // Ensure that the relative order of patches is preserved.
        result.sort();
        result
    }




    pub fn add_one_row(&self, row: &mut [&mut [f32]], position: (usize, usize), xsize: usize) {

    }
}
