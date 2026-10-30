//! Access to the cell pipeline without a window, for benchmarks and tests.
//!
//! Enabled by the `bench` feature. Not a stable interface.

use tron_core::Snapshot;
use tron_font::FontSystem;

use crate::cells::CellPipeline;
use crate::{Gpu, RenderError, Theme};

/// Padding around the grid, in pixels, as a default window has at scale 1.
const PADDING: [f32; 2] = [8.0, 8.0];
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;

/// The cell pipeline on a headless GPU device, building and uploading frames
/// the way the renderer does before it draws them.
pub struct CellBench {
    gpu: Gpu,
    cells: CellPipeline,
    theme: Theme,
    viewport: [f32; 2],
}

impl CellBench {
    /// Opens the GPU and a cell pipeline sized for a `cols` by `rows` grid.
    pub fn new(fonts: &FontSystem, cols: usize, rows: usize) -> Result<Self, RenderError> {
        let gpu = pollster::block_on(Gpu::new())?;
        let metrics = fonts.metrics();
        let cells = CellPipeline::new(&gpu.device, FORMAT, metrics, PADDING, None);
        let viewport = [
            cols as f32 * metrics.width as f32 + 2.0 * PADDING[0],
            rows as f32 * metrics.height as f32 + 2.0 * PADDING[1],
        ];
        Ok(Self { gpu, cells, theme: Theme::default(), viewport })
    }

    /// Builds the frame for `snapshot` and uploads it. Returns whether anything
    /// was uploaded.
    pub fn frame(&mut self, snapshot: &Snapshot, fonts: &mut FontSystem) -> bool {
        let Gpu { device, queue, .. } = &self.gpu;
        self.cells.prepare(device, queue, snapshot, fonts, &self.theme, true, true, self.viewport);
        let uploaded = self.cells.upload(device, queue);
        // Submitting hands the staged writes to the GPU, as every rendered frame
        // does; without it they would pile up across iterations.
        queue.submit([]);
        let _ = device.poll(wgpu::PollType::Poll);
        uploaded
    }

    /// Replaces the pipeline with a new one that has built nothing yet.
    pub fn reset(&mut self, fonts: &FontSystem) {
        self.cells = CellPipeline::new(&self.gpu.device, FORMAT, fonts.metrics(), PADDING, None);
    }

    /// Moves the grid down by part of a line, as smooth scrolling does.
    pub fn set_scroll_offset(&mut self, pixels: f32) {
        self.cells.set_scroll_offset(pixels);
    }

    /// Draws the last uploaded frame on black and reads it back: `width`,
    /// `height` and BGRA pixels, row by row.
    pub fn pixels(&self) -> (u32, u32, Vec<u8>) {
        let Gpu { device, queue, .. } = &self.gpu;
        let (width, height) = (self.viewport[0] as u32, self.viewport[1] as u32);
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bench frame"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let padded = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bench readback"),
            size: u64::from(padded * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("bench") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bench terminal"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            self.cells.draw_backgrounds(&mut pass);
            self.cells.draw_text(&mut pass);
        }
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: None },
            },
            size,
        );
        queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |result| result.expect("map readback buffer"));
        device.poll(wgpu::PollType::wait_indefinitely()).expect("GPU finished the frame");
        let mapped = buffer.slice(..).get_mapped_range().expect("readback buffer is mapped");
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for row in mapped.chunks(padded as usize) {
            pixels.extend_from_slice(&row[..(width * 4) as usize]);
        }
        (width, height, pixels)
    }
}
