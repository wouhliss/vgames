//! Direct3D 12 test game: clears a `WIDTH`×`HEIGHT` flip swap chain to the colour
//! for N frames, copies the last frame into a readback buffer and checks it (see
//! the crate docs). Uses the hardware adapter, else WARP.

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
            println!("vgames test app: d3d12 drew {} frames", options.frames);
            ExitCode::SUCCESS
        }
        Err((step, detail)) => step.fail(detail),
    }
}

#[cfg(windows)]
mod app {
    use std::mem::ManuallyDrop;

    use vgames_testapps::win::{create_window, pump_messages};
    use vgames_testapps::{HEIGHT, Options, Step, WIDTH};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
    use windows::Win32::Graphics::Direct3D12::{
        D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_QUEUE_DESC, D3D12_CPU_DESCRIPTOR_HANDLE,
        D3D12_DESCRIPTOR_HEAP_DESC, D3D12_DESCRIPTOR_HEAP_TYPE_RTV, D3D12_FENCE_FLAG_NONE,
        D3D12_HEAP_FLAG_NONE, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_READBACK,
        D3D12_PLACED_SUBRESOURCE_FOOTPRINT, D3D12_RANGE, D3D12_RESOURCE_BARRIER,
        D3D12_RESOURCE_BARRIER_0, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
        D3D12_RESOURCE_BARRIER_TYPE_TRANSITION, D3D12_RESOURCE_DESC,
        D3D12_RESOURCE_DIMENSION_BUFFER, D3D12_RESOURCE_STATE_COPY_DEST,
        D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_PRESENT,
        D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATES,
        D3D12_RESOURCE_TRANSITION_BARRIER, D3D12_TEXTURE_COPY_LOCATION,
        D3D12_TEXTURE_COPY_LOCATION_0, D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX, D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        D3D12CreateDevice, ID3D12CommandAllocator, ID3D12CommandQueue, ID3D12DescriptorHeap,
        ID3D12Device, ID3D12Fence, ID3D12GraphicsCommandList, ID3D12Resource,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
    };
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC1,
        DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIAdapter,
        IDXGIFactory4, IDXGISwapChain3,
    };
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
    use windows::core::{Interface, w};

    type Failure = (Step, String);

    fn at(step: Step) -> impl Fn(windows::core::Error) -> Failure {
        move |e| (step, e.message().to_string())
    }

    const BUFFERS: u32 = 2;

    struct Gpu {
        device: ID3D12Device,
        queue: ID3D12CommandQueue,
        fence: ID3D12Fence,
        fence_value: u64,
        event: HANDLE,
    }

    impl Gpu {
        /// Signals the fence and waits for the queue to reach it.
        #[allow(unsafe_code)]
        fn wait_idle(&mut self) -> windows::core::Result<()> {
            self.fence_value += 1;
            // SAFETY: live queue, fence and event; the event is waited on before reuse.
            unsafe {
                self.queue.Signal(&self.fence, self.fence_value)?;
                if self.fence.GetCompletedValue() < self.fence_value {
                    self.fence
                        .SetEventOnCompletion(self.fence_value, self.event)?;
                    WaitForSingleObject(self.event, INFINITE);
                }
            }
            Ok(())
        }
    }

    impl Drop for Gpu {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            // SAFETY: the event was created by `CreateEventW` and is closed once.
            unsafe {
                let _ = CloseHandle(self.event);
            }
        }
    }

    fn transition(
        resource: &ID3D12Resource,
        before: D3D12_RESOURCE_STATES,
        after: D3D12_RESOURCE_STATES,
    ) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: ManuallyDrop::new(Some(resource.clone())),
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: before,
                    StateAfter: after,
                }),
            },
            ..Default::default()
        }
    }

    /// Releases the resource reference a [`transition`] barrier holds.
    #[allow(unsafe_code)]
    fn release(barrier: D3D12_RESOURCE_BARRIER) {
        // SAFETY: the barrier was built by `transition`, so the union holds a transition.
        unsafe {
            let mut transition = ManuallyDrop::into_inner(barrier.Anonymous.Transition);
            ManuallyDrop::drop(&mut transition.pResource);
        }
    }

    #[allow(unsafe_code)]
    pub fn run(options: Options) -> Result<(), Failure> {
        let hwnd = create_window(w!("vgames test app (D3D12)")).map_err(at(Step::Window))?;
        // SAFETY: COM calls on live interfaces; every descriptor, barrier and copy
        // location is fully initialized; the readback pointer is read within the
        // footprint's row pitch and unmapped before the buffer is released.
        unsafe {
            let factory: IDXGIFactory4 =
                CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).map_err(at(Step::Device))?;
            let mut device: Option<ID3D12Device> = None;
            if D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut device).is_err() {
                let warp: IDXGIAdapter = factory.EnumWarpAdapter().map_err(at(Step::Device))?;
                D3D12CreateDevice(&warp, D3D_FEATURE_LEVEL_11_0, &mut device)
                    .map_err(at(Step::Device))?;
            }
            let device = device.ok_or((Step::Device, "no device".into()))?;
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                    ..Default::default()
                })
                .map_err(at(Step::Device))?;
            let mut gpu = Gpu {
                fence: device
                    .CreateFence(0, D3D12_FENCE_FLAG_NONE)
                    .map_err(at(Step::Device))?,
                device,
                queue,
                fence_value: 0,
                event: CreateEventW(None, false, false, None).map_err(at(Step::Device))?,
            };

            let swap_chain: IDXGISwapChain3 = factory
                .CreateSwapChainForHwnd(
                    &gpu.queue,
                    hwnd,
                    &DXGI_SWAP_CHAIN_DESC1 {
                        Width: WIDTH,
                        Height: HEIGHT,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: BUFFERS,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .and_then(|s| s.cast())
                .map_err(at(Step::SwapChain))?;
            let heap: ID3D12DescriptorHeap = gpu
                .device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                    NumDescriptors: BUFFERS,
                    ..Default::default()
                })
                .map_err(at(Step::SwapChain))?;
            let stride = gpu
                .device
                .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV)
                as usize;
            let start = heap.GetCPUDescriptorHandleForHeapStart();
            let mut targets = Vec::new();
            for i in 0..BUFFERS {
                let buffer: ID3D12Resource =
                    swap_chain.GetBuffer(i).map_err(at(Step::SwapChain))?;
                let rtv = D3D12_CPU_DESCRIPTOR_HANDLE {
                    ptr: start.ptr + i as usize * stride,
                };
                gpu.device.CreateRenderTargetView(&buffer, None, rtv);
                targets.push((buffer, rtv));
            }

            // Readback buffer for one frame (rows aligned to 256 bytes).
            let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
            let mut total = 0u64;
            let back_desc = targets
                .first()
                .map(|(b, _)| b.GetDesc())
                .ok_or((Step::SwapChain, "no buffers".into()))?;
            gpu.device.GetCopyableFootprints(
                &back_desc,
                0,
                1,
                0,
                Some(&mut footprint),
                None,
                None,
                Some(&mut total),
            );
            let mut readback: Option<ID3D12Resource> = None;
            gpu.device
                .CreateCommittedResource(
                    &D3D12_HEAP_PROPERTIES {
                        Type: D3D12_HEAP_TYPE_READBACK,
                        ..Default::default()
                    },
                    D3D12_HEAP_FLAG_NONE,
                    &D3D12_RESOURCE_DESC {
                        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                        Width: total,
                        Height: 1,
                        DepthOrArraySize: 1,
                        MipLevels: 1,
                        Format: DXGI_FORMAT_UNKNOWN,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                        ..Default::default()
                    },
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    None,
                    &mut readback,
                )
                .map_err(at(Step::Readback))?;
            let readback = readback.ok_or((Step::Readback, "no readback buffer".into()))?;

            let allocator: ID3D12CommandAllocator = gpu
                .device
                .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                .map_err(at(Step::Render))?;
            let list: ID3D12GraphicsCommandList = gpu
                .device
                .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
                .map_err(at(Step::Render))?;
            list.Close().map_err(at(Step::Render))?;

            let rgba = options.rgba();
            for frame in 0..options.frames {
                pump_messages();
                let index = swap_chain.GetCurrentBackBufferIndex() as usize;
                let (buffer, rtv) = targets
                    .get(index)
                    .ok_or((Step::Render, "bad back buffer index".into()))?;
                allocator.Reset().map_err(at(Step::Render))?;
                list.Reset(&allocator, None).map_err(at(Step::Render))?;
                let to_rt = transition(
                    buffer,
                    D3D12_RESOURCE_STATE_PRESENT,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                );
                list.ResourceBarrier(std::slice::from_ref(&to_rt));
                release(to_rt);
                list.ClearRenderTargetView(*rtv, &rgba, None);
                let last = frame + 1 == options.frames;
                if last {
                    let to_copy = transition(
                        buffer,
                        D3D12_RESOURCE_STATE_RENDER_TARGET,
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                    );
                    list.ResourceBarrier(std::slice::from_ref(&to_copy));
                    release(to_copy);
                    let dst = D3D12_TEXTURE_COPY_LOCATION {
                        pResource: ManuallyDrop::new(Some(readback.clone())),
                        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                            PlacedFootprint: footprint,
                        },
                    };
                    let src = D3D12_TEXTURE_COPY_LOCATION {
                        pResource: ManuallyDrop::new(Some(buffer.clone())),
                        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                            SubresourceIndex: 0,
                        },
                    };
                    list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
                    drop(ManuallyDrop::into_inner(dst.pResource));
                    drop(ManuallyDrop::into_inner(src.pResource));
                    let to_present = transition(
                        buffer,
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                        D3D12_RESOURCE_STATE_PRESENT,
                    );
                    list.ResourceBarrier(std::slice::from_ref(&to_present));
                    release(to_present);
                } else {
                    let to_present = transition(
                        buffer,
                        D3D12_RESOURCE_STATE_RENDER_TARGET,
                        D3D12_RESOURCE_STATE_PRESENT,
                    );
                    list.ResourceBarrier(std::slice::from_ref(&to_present));
                    release(to_present);
                }
                list.Close().map_err(at(Step::Render))?;
                gpu.queue
                    .ExecuteCommandLists(&[Some(list.cast().map_err(at(Step::Render))?)]);
                swap_chain
                    .Present(1, DXGI_PRESENT(0))
                    .ok()
                    .map_err(at(Step::Render))?;
                gpu.wait_idle().map_err(at(Step::Render))?;
            }

            let mut data = std::ptr::null_mut();
            readback
                .Map(
                    0,
                    Some(&D3D12_RANGE {
                        Begin: 0,
                        End: total as usize,
                    }),
                    Some(&mut data),
                )
                .map_err(at(Step::Readback))?;
            let base = data.cast::<u8>().add(footprint.Offset as usize);
            let px = base
                .add((HEIGHT as usize / 2) * footprint.Footprint.RowPitch as usize)
                .add((WIDTH as usize / 2) * 4);
            let pixel = [*px, *px.add(1), *px.add(2), *px.add(3)];
            readback.Unmap(0, Some(&D3D12_RANGE::default()));
            if !options.matches(pixel, true) {
                return Err((Step::WrongColor, format!("read back BGRA {pixel:?}")));
            }
        }
        Ok(())
    }
}
