//! Direct3D 11 test game: clears a `WIDTH`×`HEIGHT` swap chain to the colour for N
//! frames, reads the last frame back and checks it (see the crate docs).

use std::process::ExitCode;

#[cfg(not(windows))]
fn main() -> ExitCode {
    vgames_testapps::Step::Arguments.fail("this test game needs Windows (or Wine/Proton)")
}

#[cfg(windows)]
fn main() -> ExitCode {
    let options = match vgames_testapps::Options::parse(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => return vgames_testapps::Step::Arguments.fail(e),
    };
    match app::run(options) {
        Ok(()) => {
            println!("vgames test app: d3d11 drew {} frames", options.frames);
            ExitCode::SUCCESS
        }
        Err((step, detail)) => step.fail(detail),
    }
}

#[cfg(windows)]
mod app {
    use vgames_testapps::win::{create_window, pump_messages};
    use vgames_testapps::{HEIGHT, Options, Step, WIDTH};
    use windows::Win32::Foundation::{HMODULE, TRUE};
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
        D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
        D3D11CreateDeviceAndSwapChain, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView,
        ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_DESC, DXGI_SAMPLE_DESC,
    };
    use windows::Win32::Graphics::Dxgi::{
        DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_EFFECT_FLIP_DISCARD,
        DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGISwapChain,
    };
    use windows::core::w;

    type Failure = (Step, String);

    fn at(step: Step) -> impl Fn(windows::core::Error) -> Failure {
        move |e| (step, e.message().to_string())
    }

    #[allow(unsafe_code)]
    pub fn run(options: Options) -> Result<(), Failure> {
        let hwnd = create_window(w!("vgames test app (D3D11)")).map_err(at(Step::Window))?;
        let desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Width: WIDTH,
                Height: HEIGHT,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                ..Default::default()
            },
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            OutputWindow: hwnd,
            Windowed: TRUE,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            ..Default::default()
        };
        let (swap_chain, device, context) = create(D3D_DRIVER_TYPE_HARDWARE, &desc)
            .or_else(|_| create(D3D_DRIVER_TYPE_WARP, &desc))
            .map_err(at(Step::Device))?;
        // SAFETY: COM calls on live interfaces created above; every out-pointer is a
        // local `Option` and the mapped pointer is read within the mapped row pitch.
        unsafe {
            let back: ID3D11Texture2D = swap_chain.GetBuffer(0).map_err(at(Step::SwapChain))?;
            let mut rtv: Option<ID3D11RenderTargetView> = None;
            device
                .CreateRenderTargetView(&back, None, Some(&mut rtv))
                .map_err(at(Step::SwapChain))?;
            let rtv = rtv.ok_or((Step::SwapChain, "no render target view".into()))?;
            let mut staging_desc = D3D11_TEXTURE2D_DESC::default();
            back.GetDesc(&mut staging_desc);
            staging_desc.Usage = D3D11_USAGE_STAGING;
            staging_desc.BindFlags = 0;
            staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            staging_desc.MiscFlags = 0;
            let mut staging: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .map_err(at(Step::Readback))?;
            let staging = staging.ok_or((Step::Readback, "no staging texture".into()))?;

            let rgba = options.rgba();
            for frame in 0..options.frames {
                pump_messages();
                context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
                context.ClearRenderTargetView(&rtv, &rgba);
                if frame + 1 == options.frames {
                    context.CopyResource(&staging, &back);
                }
                swap_chain
                    .Present(1, DXGI_PRESENT(0))
                    .ok()
                    .map_err(at(Step::Render))?;
            }

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(at(Step::Readback))?;
            let row = mapped
                .pData
                .cast::<u8>()
                .add((HEIGHT as usize / 2) * mapped.RowPitch as usize);
            let px = row.add((WIDTH as usize / 2) * 4);
            let pixel = [*px, *px.add(1), *px.add(2), *px.add(3)];
            context.Unmap(&staging, 0);
            if !options.matches(pixel, true) {
                return Err((Step::WrongColor, format!("read back BGRA {pixel:?}")));
            }
        }
        Ok(())
    }

    #[allow(unsafe_code)]
    fn create(
        driver: D3D_DRIVER_TYPE,
        desc: &DXGI_SWAP_CHAIN_DESC,
    ) -> windows::core::Result<(IDXGISwapChain, ID3D11Device, ID3D11DeviceContext)> {
        let mut swap_chain = None;
        let mut device = None;
        let mut context = None;
        // SAFETY: the description and every out-pointer are valid for the call.
        unsafe {
            D3D11CreateDeviceAndSwapChain(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(desc),
                Some(&mut swap_chain),
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        match (swap_chain, device, context) {
            (Some(s), Some(d), Some(c)) => Ok((s, d, c)),
            _ => Err(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_FAIL,
            )),
        }
    }
}
