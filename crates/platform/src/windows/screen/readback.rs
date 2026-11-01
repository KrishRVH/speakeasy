use std::{num::NonZeroU32, slice};

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

pub(super) fn device_for(
    monitor: HMONITOR,
) -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
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

pub(super) struct Readback {
    source: [u32; 2],
    size: [u32; 2],
    mips: Option<Mips>,
    staging: ID3D11Texture2D,
}

struct Mips {
    texture: ID3D11Texture2D,
    view: ID3D11ShaderResourceView,
    level: NonZeroU32,
}

impl Readback {
    pub(super) fn new(
        device: &ID3D11Device,
        source: [u32; 2],
        longest_edge: u32,
    ) -> windows::core::Result<Self> {
        let size = fit(source, longest_edge);
        let mips = NonZeroU32::new(halvings(source, size))
            .map(|level| Mips::new(device, source, level))
            .transpose()?;
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
            mips,
            staging,
        })
    }

    pub(super) const fn source(&self) -> [u32; 2] {
        self.source
    }

    pub(super) const fn size(&self) -> [u32; 2] {
        self.size
    }

    #[cfg(test)]
    pub(super) fn level(&self) -> u32 {
        self.mips.as_ref().map_or(0, |mips| mips.level.get())
    }

    pub(super) fn read(
        &self,
        context: &ID3D11DeviceContext,
        frame: &ID3D11Texture2D,
        [left, top]: [u32; 2],
    ) -> windows::core::Result<Vec<u8>> {
        let [width, height] = self.source;
        let region = D3D11_BOX {
            left,
            top,
            right: left.saturating_add(width),
            bottom: top.saturating_add(height),
            back: 1,
            ..Default::default()
        };
        let first = self
            .mips
            .as_ref()
            .map_or(&self.staging, |mips| &mips.texture);

        unsafe {
            context.CopySubresourceRegion(first, 0, 0, 0, 0, frame, 0, Some(&raw const region));
            if let Some(mips) = &self.mips {
                context.GenerateMips(&mips.view);
                context.CopySubresourceRegion(
                    &self.staging,
                    0,
                    0,
                    0,
                    0,
                    &mips.texture,
                    mips.level.get(),
                    None,
                );
            }
        }
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();

        unsafe { context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped))? };
        let packed = self.pack(&mapped);

        unsafe { context.Unmap(&self.staging, 0) };
        packed
    }

    fn pack(&self, mapped: &D3D11_MAPPED_SUBRESOURCE) -> windows::core::Result<Vec<u8>> {
        let [width, height] = self.size.map(|edge| edge as usize);
        let row = width.saturating_mul(BYTES_PER_PIXEL);
        let pitch = mapped.RowPitch as usize;
        if pitch < row || mapped.pData.is_null() {
            return Err(E_UNEXPECTED.into());
        }
        let length = mapped_len(pitch, row, height).ok_or(E_UNEXPECTED)?;

        let bytes = unsafe { slice::from_raw_parts(mapped.pData.cast::<u8>(), length) };
        pack_rows(bytes, pitch, width, height).ok_or_else(|| E_UNEXPECTED.into())
    }
}

impl Mips {
    fn new(
        device: &ID3D11Device,
        source: [u32; 2],
        level: NonZeroU32,
    ) -> windows::core::Result<Self> {
        let texture = create_texture(
            device,
            &D3D11_TEXTURE2D_DESC {
                BindFlags: (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET)
                    .0
                    .cast_unsigned(),
                MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0.cast_unsigned(),
                ..texture_description(source, level.get().saturating_add(1))
            },
        )?;
        let mut view = None;

        unsafe { device.CreateShaderResourceView(&texture, None, Some(&raw mut view))? };
        Ok(Self {
            texture,
            view: created(view)?,
            level,
        })
    }
}

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

    unsafe { device.CreateTexture2D(description, None, Some(&raw mut texture))? };
    created(texture)
}

pub(super) fn created<T>(value: Option<T>) -> windows::core::Result<T> {
    value.ok_or_else(|| E_UNEXPECTED.into())
}

fn halvings([width, _]: [u32; 2], [fitted, _]: [u32; 2]) -> u32 {
    width
        .checked_div(fitted)
        .and_then(u32::checked_ilog2)
        .unwrap_or(0)
}

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
        let mapped = [1, 1, 1, 1, 2, 2, 2, 2, 9, 9, 9, 9, 3, 3, 3, 3, 4, 4, 4, 4];
        assert_eq!(mapped_len(12, 8, 2), Some(mapped.len()));
        assert_eq!(
            pack_rows(&mapped, 12, 2, 2),
            Some(vec![1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4])
        );
        assert_eq!(mapped_len(12, 8, 0), None, "An empty image maps nothing");
    }
}
