//! Screen capture through DXGI Desktop Duplication, shrunk on the GPU.
//!
//! Each new desktop frame is copied into a mipmapped texture and the GPU averages it down
//! (GenerateMips is a box filter), so only a ~160 pixel wide image ever crosses to the CPU:
//! a few dozen kilobytes per frame instead of tens of megabytes. AcquireNextFrame blocks until
//! something on screen actually changes, so a still screen costs nothing at all.

use anyhow::{Context, Result, bail};
use lightsync_core::zones::Frame;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::*;
use windows_core::Interface;

pub struct Capture {
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    mips: ID3D11Texture2D,
    view: ID3D11ShaderResourceView,
    staging: ID3D11Texture2D,
    level: u32,
    small: (u32, u32),
    previous: Option<Frame>,
    pub size: (u32, u32),
    pub monitor_name: String,
}

/// Every display Windows knows, in order, as "name (width × height)".
pub fn monitors() -> Vec<String> {
    let mut names = Vec::new();
    let _ = for_each_output(|output| {
        if let Ok(desc) = unsafe { output.GetDesc() } {
            names.push(describe(&desc));
        }
        false
    });
    names
}

fn describe(desc: &DXGI_OUTPUT_DESC) -> String {
    let r = desc.DesktopCoordinates;
    let name = String::from_utf16_lossy(&desc.DeviceName).trim_end_matches('\0').replace(r"\\.\", "");
    format!("{name} ({} × {})", r.right - r.left, r.bottom - r.top)
}

/// Calls `visit` for each output of each adapter until it returns true.
fn for_each_output(mut visit: impl FnMut(&IDXGIOutput) -> bool) -> Result<Option<(IDXGIAdapter1, IDXGIOutput)>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    for a in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(a) }) else { break };
        for o in 0.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(o) }) else { break };
            if visit(&output) {
                return Ok(Some((adapter, output)));
            }
        }
    }
    Ok(None)
}

impl Capture {
    /// `detail` is the rough width the screen is averaged down to.
    pub fn open(monitor: u32, detail: u32) -> Result<Self> {
        let mut index = 0;
        let (adapter, output) = for_each_output(|_| {
            index += 1;
            index - 1 == monitor
        })?
        .with_context(|| format!("display {} isn't connected", monitor + 1))?;
        let monitor_name = describe(&unsafe { output.GetDesc()? });
        let output: IDXGIOutput1 = output.cast()?;

        let (mut device, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .context("couldn't start Direct3D")?;
        }
        let device = device.context("no Direct3D device")?;
        let context = context.context("no Direct3D context")?;
        let duplication = unsafe { output.DuplicateOutput(&device) }
            .context("Windows wouldn't share the screen (another app may be duplicating it)")?;
        let desc = unsafe { duplication.GetDesc() };
        let (width, height) = (desc.ModeDesc.Width, desc.ModeDesc.Height);
        if width == 0 || height == 0 {
            bail!("the display reports no size");
        }

        // The smallest mip level that's still at least `detail` wide
        let mut level = 0;
        while (width >> (level + 1)) >= detail.max(16) && (height >> (level + 1)) >= 8 {
            level += 1;
        }
        let small = ((width >> level).max(1), (height >> level).max(1));

        let mips_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: level + 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32,
        };
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: small.0,
            Height: small.1,
            MipLevels: 1,
            BindFlags: 0,
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
            ..mips_desc
        };
        let (mut mips, mut staging, mut view) = (None, None, None);
        unsafe {
            device.CreateTexture2D(&mips_desc, None, Some(&mut mips))?;
            device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
        }
        let mips = mips.context("no mip texture")?;
        unsafe { device.CreateShaderResourceView(&mips, None, Some(&mut view))? };

        Ok(Self {
            context,
            duplication,
            mips,
            view: view.context("no shader view")?,
            staging: staging.context("no staging texture")?,
            level,
            small,
            previous: None,
            size: (width, height),
            monitor_name,
        })
    }

    /// Waits up to `timeout_ms` for the screen to change. `None` means nothing changed
    /// (only the mouse moved, or the wait ran out). An error means the capture must be reopened,
    /// which happens on resolution changes, the lock screen and UAC prompts.
    pub fn next(&mut self, timeout_ms: u32) -> Result<Option<Frame>> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        if let Err(e) = unsafe { self.duplication.AcquireNextFrame(timeout_ms, &mut info, &mut resource) } {
            if e.code() == DXGI_ERROR_WAIT_TIMEOUT {
                return Ok(None);
            }
            return Err(e).context("the screen capture was interrupted");
        }
        let frame = if info.LastPresentTime == 0 {
            Ok(None) // just the pointer
        } else {
            resource.context("no desktop image").and_then(|r| self.shrink(&r.cast()?)).map(Some)
        };
        unsafe {
            let _ = self.duplication.ReleaseFrame();
        }
        // A blinking caret or a tooltip changes the desktop but not the averaged picture
        match frame? {
            Some(frame) if self.previous.as_ref() != Some(&frame) => {
                self.previous = Some(frame.clone());
                Ok(Some(frame))
            }
            _ => Ok(None),
        }
    }

    fn shrink(&self, desktop: &ID3D11Texture2D) -> Result<Frame> {
        let (w, h) = (self.small.0 as usize, self.small.1 as usize);
        let mut bgra = vec![0u8; w * h * 4];
        unsafe {
            self.context.CopySubresourceRegion(&self.mips, 0, 0, 0, 0, desktop, 0, None);
            self.context.GenerateMips(&self.view);
            self.context.CopySubresourceRegion(&self.staging, 0, 0, 0, 0, &self.mips, self.level, None);
            // Waiting inside Map spins a core until the GPU is done, so poll and sleep instead
            self.context.Flush();
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            let mut tries = 0;
            while let Err(e) = self.context.Map(
                &self.staging,
                0,
                D3D11_MAP_READ,
                D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                Some(&mut mapped),
            ) {
                tries += 1;
                if e.code() != DXGI_ERROR_WAS_STILL_DRAWING || tries > 200 {
                    return Err(e.into());
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let source = mapped.pData as *const u8;
            for y in 0..h {
                let row = std::slice::from_raw_parts(source.add(y * mapped.RowPitch as usize), w * 4);
                bgra[y * w * 4..(y + 1) * w * 4].copy_from_slice(row);
            }
            self.context.Unmap(&self.staging, 0);
        }
        Ok(Frame { width: w, height: h, bgra })
    }
}
