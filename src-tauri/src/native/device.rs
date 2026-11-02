// Графическое устройство нативных окон: D3D11 + Direct2D + DirectComposition.
//
// Всё, что зависит от видеокарты, живёт здесь и пересоздаётся целиком при потере устройства
// (смена или сброс драйвера, выход из сна, удалённый рабочий стол). Фабрика Direct2D от
// устройства не зависит и переживает пересоздание.

use windows::core::{Interface, Result};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1,
    D2D1_DEBUG_LEVEL_NONE, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_OPTIONS,
    D2D1_FACTORY_TYPE_SINGLE_THREADED,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{DCompositionCreateDevice, IDCompositionDevice};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGIFactory2};

pub fn create_d2d_factory() -> Result<ID2D1Factory1> {
    let options = D2D1_FACTORY_OPTIONS { debugLevel: D2D1_DEBUG_LEVEL_NONE };
    unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, Some(&options)) }
}

pub struct Gfx {
    pub d3d: ID3D11Device,
    pub dxgi_factory: IDXGIFactory2,
    pub dc: ID2D1DeviceContext,
    pub dcomp: IDCompositionDevice,
    /// Растёт при каждом пересоздании: по нему окна и кэши понимают, что их ресурсы устарели.
    pub generation: u64,
    /// Устройство программное (WARP) — видеокарты нет или она отказала.
    pub software: bool,
    _d2d_device: ID2D1Device,
}

impl Gfx {
    pub fn new(factory: &ID2D1Factory1, generation: u64) -> Result<Self> {
        let (d3d, software) = match create_d3d(D3D_DRIVER_TYPE_HARDWARE) {
            Ok(d) => (d, false),
            Err(_) => (create_d3d(D3D_DRIVER_TYPE_WARP)?, true),
        };
        let dxgi_device: IDXGIDevice = d3d.cast()?;
        let dxgi_factory: IDXGIFactory2 = unsafe { dxgi_device.GetAdapter()?.GetParent()? };
        let d2d_device = unsafe { factory.CreateDevice(&dxgi_device)? };
        let dc = unsafe { d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)? };
        let dcomp: IDCompositionDevice = unsafe { DCompositionCreateDevice(&dxgi_device)? };
        Ok(Self { d3d, dxgi_factory, dc, dcomp, generation, software, _d2d_device: d2d_device })
    }

    /// Устройство потеряно (драйвер сброшен или видеокарту убрали).
    pub fn is_lost(&self) -> bool {
        unsafe { self.d3d.GetDeviceRemovedReason().is_err() }
    }
}

fn create_d3d(driver: D3D_DRIVER_TYPE) -> Result<ID3D11Device> {
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    device.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))
}
