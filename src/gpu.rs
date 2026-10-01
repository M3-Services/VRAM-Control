//! GPU adapter enumeration through DXGI.

use crate::model::{GpuInfo, Luid};
use anyhow::{Context, Result};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
};

/// Lists the hardware GPU adapters. Software adapters (such as the basic render driver) are skipped.
pub fn enumerate_gpus() -> Result<Vec<GpuInfo>> {
    // SAFETY: plain COM calls on a factory created and owned by this function.
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().context("failed to create the DXGI factory")?;
        let mut gpus = Vec::new();
        let mut i = 0;
        // EnumAdapters1 returns an error (DXGI_ERROR_NOT_FOUND) after the last adapter.
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let desc = adapter
                .GetDesc1()
                .context("failed to read an adapter description")?;
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let name_len = desc
                .Description
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(desc.Description.len());
            gpus.push(GpuInfo {
                index: gpus.len(),
                name: String::from_utf16_lossy(&desc.Description[..name_len]),
                luid: Luid::new(desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart),
                total_bytes: desc.DedicatedVideoMemory as u64,
            });
        }
        Ok(gpus)
    }
}
