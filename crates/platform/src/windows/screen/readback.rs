//! The Direct3D 11 device on the adapter that drives the display, and reading capture frames back
//! to memory, halved on the GPU until they fit.

use std::slice;

use windows::Win32::{
    Foundation::{E_UNEXPECTED, HMODULE},
    Graphics::{
        Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN},
        Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CPU_ACCESS_READ,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
            D3D11_RESOURCE_MISC_GENERATE_MIPS, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
            ID3D11ShaderResourceView, ID3D11Texture2D,
        },
        Dxgi::{
            Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
            CreateDXGIFactory1, IDXGIFactory1,
        },
        Gdi::HMONITOR,
    },
};

use crate::screen::geometry::{fit, pack_rows};

pub(super) const BYTES_PER_PIXEL: usize = 4;

/// A Direct3D 11 device on the adapter that drives `monitor`, so frames need no copy between
/// adapters; the default adapter serves a display that no adapter reports.
pub(super) fn device_for(
    monitor: HMONITOR,
) -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
    // SAFETY: enumeration only reads descriptions through the live interfaces DXGI returns.
    let adapter = unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        (0..)
            .map_while(|index| factory.EnumAdapters(index).ok())
            .find(|adapter| {
                (0..)
                    .map_while(|index| adapter.EnumOutputs(index).ok())
                    .any(|output| {
                        output
                            .GetDesc()
                            .is_ok_and(|description| description.Monitor == monitor)
                    })
            })
    };
    let driver = if adapter.is_some() {
        D3D_DRIVER_TYPE_UNKNOWN
    } else {
        D3D_DRIVER_TYPE_HARDWARE
    };
    let (mut device, mut context) = (None, None);
    // SAFETY: any adapter is live, and both out-parameters are live locals.
    unsafe {
        D3D11CreateDevice(
            adapter.as_ref(),
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&raw mut device),
            None,
            Some(&raw mut context),
        )?;
    }
    Ok((created(device)?, created(context)?))
}

/// Reads frames of one size back to memory, halved on the GPU until they fit.
pub(super) struct Readback {
    /// The size of the frames read.
    pub(super) source: [u32; 2],
    /// The size delivered: that of the first mip level whose long edge fits.
    pub(super) size: [u32; 2],
    /// That mip level, zero when the frame already fits.
    pub(super) level: u32,
    /// The frame and its halvings, while it needs halving.
    mips: Option<Mips>,
    /// A CPU-readable copy of the delivered level, reused frame after frame.
    staging: ID3D11Texture2D,
}

struct Mips {
    texture: ID3D11Texture2D,
    view: ID3D11ShaderResourceView,
}

impl Readback {
    pub(super) fn new(
        device: &ID3D11Device,
        source: [u32; 2],
        longest_edge: u32,
    ) -> windows::core::Result<Self> {
        let size = fit(source, longest_edge);
        let level = halvings(source, size);
        let mips = if level == 0 {
            None
        } else {
            Some(Mips::new(device, source, level)?)
        };
        let staging = create_texture(
            device,
            &D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0.cast_unsigned(),
                ..texture_description(size, 1)
            },
        )?;
        Ok(Self {
            source,
            size,
            level,
            mips,
            staging,
        })
    }

    /// Copies `frame`, which must be of the source size, to memory as tightly packed BGRA rows of
    /// the delivered size.
    pub(super) fn read(
        &self,
        context: &ID3D11DeviceContext,
        frame: &ID3D11Texture2D,
    ) -> windows::core::Result<Vec<u8>> {
        let [width, height] = self.source;
        let region = D3D11_BOX {
            right: width,
            bottom: height,
            back: 1,
            ..Default::default()
        };
        let first = self
            .mips
            .as_ref()
            .map_or(&self.staging, |mips| &mips.texture);
        // SAFETY: every texture belongs to this context's device and shares the frame's format,
        // and the region spans the frame, whose size is the source size.
        unsafe {
            context.CopySubresourceRegion(first, 0, 0, 0, 0, frame, 0, Some(&raw const region));
            if let Some(mips) = &self.mips {
                context.GenerateMips(&mips.view);
                let (staging, halved, level) = (&self.staging, &mips.texture, self.level);
                context.CopySubresourceRegion(staging, 0, 0, 0, 0, halved, level, None);
            }
        }
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the staging texture allows CPU reads; mapping waits for the copies above.
        unsafe { context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped))? };
        let packed = self.pack(&mapped);
        // SAFETY: the texture is mapped, and `pack` no longer borrows its bytes.
        unsafe { context.Unmap(&self.staging, 0) };
        packed
    }

    fn pack(&self, mapped: &D3D11_MAPPED_SUBRESOURCE) -> windows::core::Result<Vec<u8>> {
        let [width, height] = self.size.map(|edge| edge as usize);
        let row = width.saturating_mul(BYTES_PER_PIXEL);
        let pitch = mapped.RowPitch as usize;
        let Some(length) =
            mapped_len(pitch, row, height).filter(|_| pitch >= row && !mapped.pData.is_null())
        else {
            return Err(E_UNEXPECTED.into());
        };
        // SAFETY: Direct3D maps `height` rows starting `pitch` bytes apart, valid until the caller
        // unmaps them, and `length` ends with the last row's pixels.
        let bytes = unsafe { slice::from_raw_parts(mapped.pData.cast::<u8>(), length) };
        pack_rows(bytes, pitch, width, height).ok_or_else(|| E_UNEXPECTED.into())
    }
}

impl Mips {
    fn new(device: &ID3D11Device, source: [u32; 2], level: u32) -> windows::core::Result<Self> {
        let texture = create_texture(
            device,
            &D3D11_TEXTURE2D_DESC {
                BindFlags: (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET)
                    .0
                    .cast_unsigned(),
                MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0.cast_unsigned(),
                ..texture_description(source, level.saturating_add(1))
            },
        )?;
        let mut view = None;
        // SAFETY: the texture is live and the out-parameter outlives the call.
        unsafe { device.CreateShaderResourceView(&texture, None, Some(&raw mut view))? };
        Ok(Self {
            texture,
            view: created(view)?,
        })
    }
}

/// A single-sampled BGRA texture of `size` with `levels` mip levels.
pub(super) fn texture_description([width, height]: [u32; 2], levels: u32) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: levels,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        ..Default::default()
    }
}

fn create_texture(
    device: &ID3D11Device,
    description: &D3D11_TEXTURE2D_DESC,
) -> windows::core::Result<ID3D11Texture2D> {
    let mut texture = None;
    // SAFETY: the description and out-parameter outlive the call, which reads no initial data.
    unsafe { device.CreateTexture2D(description, None, Some(&raw mut texture))? };
    created(texture)
}

/// A creation call that succeeds always fills its out-parameter.
pub(super) fn created<T>(value: Option<T>) -> windows::core::Result<T> {
    value.ok_or_else(|| E_UNEXPECTED.into())
}

/// How many times `fit` halved `source` to reach `fitted`, which is the mip level holding it.
fn halvings([width, _]: [u32; 2], [fitted, _]: [u32; 2]) -> u32 {
    width
        .checked_div(fitted)
        .and_then(u32::checked_ilog2)
        .unwrap_or(0)
}

/// The bytes a mapping of `rows` rows must hold: every row but the last spans the full pitch.
fn mapped_len(pitch: usize, row: usize, rows: usize) -> Option<usize> {
    pitch.checked_mul(rows.checked_sub(1)?)?.checked_add(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitted_frames_are_the_mip_level_their_halvings_name() {
        for (source, longest_edge, level) in [
            ([1920, 1080], 1920, 0),
            ([3840, 2160], 2560, 1),
            ([3840, 2160], 960, 2),
            ([1921, 1081], 960, 1),
            ([1080, 1920], 960, 1),
            ([4096, 3], 16, 1),
        ] {
            let fitted = fit(source, longest_edge);
            assert_eq!(
                halvings(source, fitted),
                level,
                "{source:?} fitted to {fitted:?}"
            );
            assert_eq!(
                fitted,
                source.map(|edge| edge >> level),
                "The staging texture must match mip level {level} of {source:?}"
            );
        }
    }

    #[test]
    fn a_mapping_ends_with_its_last_row_of_pixels() {
        // Two rows of two pixels, each starting 12 bytes after the last; the last row is unpadded.
        let mapped = [1, 1, 1, 1, 2, 2, 2, 2, 9, 9, 9, 9, 3, 3, 3, 3, 4, 4, 4, 4];
        assert_eq!(mapped_len(12, 8, 2), Some(mapped.len()));
        assert_eq!(
            pack_rows(&mapped, 12, 2, 2),
            Some(vec![1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4])
        );
        assert_eq!(mapped_len(12, 8, 0), None, "An empty image maps nothing");
    }
}
