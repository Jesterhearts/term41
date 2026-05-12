use super::FgGeometry;
use super::FgVertex;
use super::ImageGeometry;
use super::ImageVertex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PageDrawRange {
    pub(super) page_index: usize,
    pub(super) index_start: u32,
    pub(super) index_count: u32,
    pub(super) vertex_base: i32,
}

#[derive(Default)]
pub(super) struct UploadBuffer {
    pub(super) buffer: Option<wgpu::Buffer>,
    pub(super) capacity: u64,
}

impl UploadBuffer {
    pub(super) fn write<T: bytemuck::Pod>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        usage: wgpu::BufferUsages,
        data: &[T],
    ) -> bool {
        let bytes = bytemuck::cast_slice(data);
        if bytes.is_empty() {
            return false;
        }

        let needed = bytes.len() as u64;
        if self.capacity < needed {
            self.capacity = next_upload_buffer_size(needed);
            self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: self.capacity,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }

        if let Some(buffer) = &self.buffer {
            queue.write_buffer(buffer, 0, bytes);
            true
        } else {
            false
        }
    }

    pub(super) fn buffer(&self) -> Option<&wgpu::Buffer> {
        self.buffer.as_ref()
    }
}

pub(super) fn next_upload_buffer_size(needed: u64) -> u64 {
    needed.next_power_of_two().max(4096)
}

#[derive(Default)]
pub(super) struct GeometryUpload {
    pub(super) vertex_buffer: UploadBuffer,
    pub(super) index_buffer: UploadBuffer,
    pub(super) has_indices: bool,
    pub(super) index_count: u32,
}

impl GeometryUpload {
    pub(super) fn upload<V: bytemuck::Pod>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertex_label: &'static str,
        index_label: &'static str,
        vertices: &[V],
        indices: &[u32],
    ) {
        self.vertex_buffer.write(
            device,
            queue,
            vertex_label,
            wgpu::BufferUsages::VERTEX,
            vertices,
        );
        self.has_indices = self.index_buffer.write(
            device,
            queue,
            index_label,
            wgpu::BufferUsages::INDEX,
            indices,
        );
        self.index_count = indices.len() as u32;
    }
}

pub(super) struct PageGeometryUpload<V> {
    pub(super) vertices: Vec<V>,
    pub(super) indices: Vec<u32>,
    pub(super) ranges: Vec<PageDrawRange>,
    pub(super) vertex_buffer: UploadBuffer,
    pub(super) index_buffer: UploadBuffer,
    pub(super) has_vertices: bool,
    pub(super) has_indices: bool,
}

impl<V> Default for PageGeometryUpload<V> {
    fn default() -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
            ranges: Vec::new(),
            vertex_buffer: UploadBuffer::default(),
            index_buffer: UploadBuffer::default(),
            has_vertices: false,
            has_indices: false,
        }
    }
}

impl<V: bytemuck::Pod + Copy> PageGeometryUpload<V> {
    pub(super) fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.ranges.clear();
        self.has_vertices = false;
        self.has_indices = false;
    }

    pub(super) fn push_batch(
        &mut self,
        page_index: usize,
        vertices: &[V],
        indices: &[u32],
    ) {
        if indices.is_empty() {
            return;
        }

        let vertex_base = self.vertices.len() as i32;
        let index_start = self.indices.len() as u32;
        self.vertices.extend_from_slice(vertices);
        self.indices.extend_from_slice(indices);
        self.ranges.push(PageDrawRange {
            page_index,
            index_start,
            index_count: indices.len() as u32,
            vertex_base,
        });
    }

    pub(super) fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertex_label: &'static str,
        index_label: &'static str,
    ) {
        self.has_vertices = self.vertex_buffer.write(
            device,
            queue,
            vertex_label,
            wgpu::BufferUsages::VERTEX,
            &self.vertices,
        );
        self.has_indices = self.index_buffer.write(
            device,
            queue,
            index_label,
            wgpu::BufferUsages::INDEX,
            &self.indices,
        );
    }

    pub(super) fn is_drawable(&self) -> bool {
        self.has_vertices && self.has_indices && !self.ranges.is_empty()
    }
}

pub(super) fn upload_fg_geometry(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    upload: &mut PageGeometryUpload<FgVertex>,
    geometry: &FgGeometry,
) {
    upload.clear();
    for batch in &geometry.batches {
        upload.push_batch(batch.page_index, &batch.vertices, &batch.indices);
    }
    upload.upload(device, queue, "fg_verts", "fg_idx");
}

pub(super) fn upload_image_geometry(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    upload: &mut PageGeometryUpload<ImageVertex>,
    geometry: &ImageGeometry,
) {
    upload.clear();
    for batch in &geometry.batches {
        upload.push_batch(batch.page_index, &batch.vertices, &batch.indices);
    }
    upload.upload(device, queue, "img_verts", "img_idx");
}

#[derive(Default)]
pub(super) struct RendererUploads {
    pub(super) terminal_clear: GeometryUpload,
    pub(super) terminal_bg: GeometryUpload,
    pub(super) bg: GeometryUpload,
    pub(super) overlay_bg: GeometryUpload,
    pub(super) top_overlay_bg: GeometryUpload,
    pub(super) terminal_fg: PageGeometryUpload<FgVertex>,
    pub(super) fg: PageGeometryUpload<FgVertex>,
    pub(super) overlay_fg: PageGeometryUpload<FgVertex>,
    pub(super) top_overlay_fg: PageGeometryUpload<FgVertex>,
    pub(super) under_image: PageGeometryUpload<ImageVertex>,
    pub(super) over_image: PageGeometryUpload<ImageVertex>,
}
