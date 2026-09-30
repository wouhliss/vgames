/*
 * D3D11 smoke test for compatibility runtimes (A5-T12, runtimes.yml).
 *
 * Creates a hardware D3D11 device, clears a render target to a known color,
 * copies it to a staging texture and checks the pixel on the CPU. Under Proton
 * this exercises DXVK (Vulkan); under Wine on macOS, DXMT or D3DMetal (Metal).
 *
 * Exit codes: 0 = rendered correctly, 1 = wrong result or API failure after the
 * device exists, 2 = no D3D11 hardware device (the runner has no usable GPU API).
 *
 * Build: x86_64-w64-mingw32-gcc -O2 -Wall -Wextra -o d3d11_smoke.exe d3d11_smoke.c -ld3d11
 */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>
#include <stdlib.h>

#define SIZE 4

static int fail(const char *what, HRESULT hr) {
    printf("d3d11-smoke: %s failed (hr=0x%08lx)\n", what, (unsigned long)hr);
    return 1;
}

static int close_to(unsigned value, unsigned expected) {
    return value + 2 >= expected && value <= expected + 2;
}

int main(void) {
    static const D3D_FEATURE_LEVEL levels[] = {D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_0};
    ID3D11Device *device = NULL;
    ID3D11DeviceContext *context = NULL;
    D3D_FEATURE_LEVEL level = 0;
    HRESULT hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, levels,
                                   sizeof(levels) / sizeof(levels[0]), D3D11_SDK_VERSION,
                                   &device, &level, &context);
    if (FAILED(hr)) {
        printf("d3d11-smoke: no hardware D3D11 device (hr=0x%08lx)\n", (unsigned long)hr);
        return 2;
    }
    printf("d3d11-smoke: device created, feature level 0x%x\n", (unsigned)level);

    D3D11_TEXTURE2D_DESC desc;
    ZeroMemory(&desc, sizeof(desc));
    desc.Width = SIZE;
    desc.Height = SIZE;
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = D3D11_BIND_RENDER_TARGET;
    ID3D11Texture2D *target = NULL;
    if (FAILED(hr = ID3D11Device_CreateTexture2D(device, &desc, NULL, &target)))
        return fail("CreateTexture2D(render target)", hr);

    ID3D11RenderTargetView *view = NULL;
    if (FAILED(hr = ID3D11Device_CreateRenderTargetView(device, (ID3D11Resource *)target, NULL,
                                                        &view)))
        return fail("CreateRenderTargetView", hr);

    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    ID3D11Texture2D *staging = NULL;
    if (FAILED(hr = ID3D11Device_CreateTexture2D(device, &desc, NULL, &staging)))
        return fail("CreateTexture2D(staging)", hr);

    const float color[4] = {0.25f, 0.5f, 0.75f, 1.0f};
    ID3D11DeviceContext_ClearRenderTargetView(context, view, color);
    ID3D11DeviceContext_CopyResource(context, (ID3D11Resource *)staging,
                                     (ID3D11Resource *)target);

    D3D11_MAPPED_SUBRESOURCE mapped;
    if (FAILED(hr = ID3D11DeviceContext_Map(context, (ID3D11Resource *)staging, 0, D3D11_MAP_READ,
                                            0, &mapped)))
        return fail("Map", hr);
    const unsigned char *px = (const unsigned char *)mapped.pData;
    unsigned r = px[0], g = px[1], b = px[2], a = px[3];
    ID3D11DeviceContext_Unmap(context, (ID3D11Resource *)staging, 0);

    ID3D11Texture2D_Release(staging);
    ID3D11RenderTargetView_Release(view);
    ID3D11Texture2D_Release(target);
    ID3D11DeviceContext_Release(context);
    ID3D11Device_Release(device);

    /* 0.25, 0.5, 0.75, 1.0 in UNORM8: 64, 128, 191, 255 (± rounding). */
    if (close_to(r, 64) && close_to(g, 128) && close_to(b, 191) && a == 255) {
        printf("d3d11-smoke: ok (%u, %u, %u, %u)\n", r, g, b, a);
        return 0;
    }
    printf("d3d11-smoke: wrong pixel (%u, %u, %u, %u), expected (64, 128, 191, 255)\n", r, g,
           b, a);
    return 1;
}
