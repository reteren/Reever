// Хромовая рамка главного окна: тот же фрагментный шейдер, что у skin/frame.js (рендер из
// Blender — цвет и карта нормалей — переосвещается тремя источниками: блик курсора, свет линзы,
// бегущая волна; плюс подсветка лунок и налив хрома от линзы), переписанный на HLSL. Рисуется
// пиксельным шейдером D3D11 в свою текстуру перед кадром (WindowHandler::prepare), в кадре
// текстура кладётся под экраны как битмап Direct2D.
//
// Как и в вебе, шейдер запускается только когда освещение изменилось (`FrameLight::key`):
// в статическом режиме (shell.js FRAME_STATIC) — один раз на размер и устройство.

use std::sync::OnceLock;

use windows::core::{Interface, Result, PCSTR};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap1, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Buffer, ID3D11PixelShader, ID3D11RenderTargetView, ID3D11SamplerState, ID3D11ShaderResourceView,
    ID3D11Texture2D, ID3D11VertexShader, D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_BUFFER_DESC, D3D11_COMPARISON_NEVER, D3D11_FILTER_MIN_MAG_MIP_LINEAR,
    D3D11_SAMPLER_DESC, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_TEXTURE_ADDRESS_CLAMP,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_IMMUTABLE, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGISurface;

use crate::native::GpuCx;

/// Освещение кадра — как `render(s)` в frame.js. Координаты — в пикселях скина (1280×880).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameLight {
    pub mouse: (f32, f32),
    pub mouse_on: f32,
    pub orb: (f32, f32),
    pub orb_light: f32,
    pub sweep_x: f32,
    pub sweep_i: f32,
    pub reveal: f32,
    /// x, y, подсветка — для трёх лунок
    pub lunki: [[f32; 3]; 3],
}

impl FrameLight {
    /// Ключ кадра: значения, округлённые до тысячных, — как lastKey в frame.js.
    pub fn key(&self) -> Vec<i32> {
        let q = |v: f32| (v * 1000.0).round() as i32;
        let mut k = vec![
            q(self.mouse.0), q(self.mouse.1), q(self.mouse_on), q(self.orb.0), q(self.orb.1),
            q(self.orb_light), q(self.sweep_x), q(self.sweep_i), q(self.reveal),
        ];
        for l in &self.lunki {
            k.extend(l.iter().map(|v| q(*v)));
        }
        k
    }
}

/// Константный буфер: выравнивание по 16 байт — как `cbuffer P` в HLSL.
#[repr(C)]
#[derive(Clone, Copy)]
struct Constants {
    canvas: [f32; 2],
    skin: [f32; 2],
    mouse: [f32; 2],
    mouse_on: f32,
    _p0: f32,
    orb: [f32; 2],
    orb_i: f32,
    sweep: f32,
    sweep_i: f32,
    reveal: f32,
    _p1: [f32; 2],
    lunki: [[f32; 4]; 3],
}

const HLSL: &str = r#"
cbuffer P : register(b0) {
  float2 uCanvas; float2 uSkin;
  float2 uMouse; float uMouseOn; float uP0;
  float2 uOrb; float uOrbI; float uSweep;
  float uSweepI; float uReveal; float2 uP1;
  float4 uLunki[3];
};
Texture2D tColor : register(t0);
Texture2D tNormal : register(t1);
SamplerState sLinear : register(s0);
static const float3 uAcc = float3(39.0, 230.0, 170.0) / 255.0;

float4 vs(uint id : SV_VertexID) : SV_Position {
  float2 p = id == 0 ? float2(-1, -1) : id == 1 ? float2(-1, 3) : float2(3, -1);
  return float4(p, 0, 1);
}

// картинки приходят premultiplied (WIC 32bppPBGRA), шейдер считает по чистому цвету
float4 unpremul(float4 c) { return float4(c.a > 0.0 ? c.rgb / c.a : float3(0, 0, 0), c.a); }

float4 ps(float4 pos : SV_Position) : SV_Target {
  // SV_Position — сверху вниз, как uv после переворота в frame.js
  float2 uv = pos.xy / uCanvas;
  float2 p = uv * uSkin;
  float4 c = unpremul(tColor.Sample(sLinear, uv));
  float a = c.a;
  if (a < 0.003) discard;
  float3 n = normalize(unpremul(tNormal.Sample(sLinear, uv)).xyz * 2.0 - 1.0);
  float3 V = float3(0.0, 0.0, 1.0);
  float3 add = float3(0.0, 0.0, 0.0);

  // курсор — белый точечный свет над рамкой
  float3 Lc = float3(uMouse.x - p.x, -(uMouse.y - p.y), 230.0);
  float dc = length(Lc); Lc /= dc;
  float nh = max(dot(n, normalize(Lc + V)), 0.0);
  float fc = 1.0 / (1.0 + pow(dc / 380.0, 2.0));
  add += float3(1.0, 1.0, 0.98) * (pow(nh, 110.0) * 1.25 + pow(nh, 14.0) * 0.10) * fc * uMouseOn;

  // линза — низкий бирюзовый свет, скользит по хрому рядом
  float3 Lo = float3(uOrb.x - p.x, -(uOrb.y - p.y), 36.0);
  float d2 = length(Lo); Lo /= d2;
  float lam = max(dot(n, Lo), 0.0);
  float so = pow(max(dot(n, normalize(Lo + V)), 0.0), 34.0);
  float fo = 1.0 / (1.0 + pow(d2 / 190.0, 2.0));
  add += uAcc * (lam * 0.32 + so * 1.7) * fo * uOrbI;

  // световая волна — вертикальная полоса света, бегущая по рамке
  float sx = uSweep * uSkin.x;
  float3 Ls = normalize(float3(sx - p.x, 40.0, 150.0));
  float ss = pow(max(dot(n, normalize(Ls + V)), 0.0), 60.0) * exp(-pow((p.x - sx) / 220.0, 2.0));
  add += lerp(float3(1.0, 1.0, 1.0), uAcc, 0.35) * ss * uSweepI * 1.4;

  // лунки — подсветка изнутри при наведении
  [unroll] for (int i = 0; i < 3; i++) {
    float3 lk = uLunki[i].xyz;
    if (lk.z > 0.001) {
      float2 d = p - lk.xy;
      float f = exp(-dot(d, d) / (2.0 * 17.0 * 17.0));
      float3 Ll = normalize(float3(-d.x, d.y, 6.0));
      add += uAcc * lk.z * (max(dot(n, Ll), 0.0) * 1.2 + 0.25) * f;
    }
  }

  // на полупрозрачных кончиках шипов нормаль недостоверна — блики гасим, иначе искрят
  float3 col = c.rgb + add * smoothstep(0.3, 0.9, a);

  // налив хрома от линзы: фронт светится, за фронтом — металл
  float dist = length(p - uOrb);
  float front = uReveal - dist;
  float shown = smoothstep(-6.0, 24.0, front);
  float edge = exp(-pow(front / 26.0, 2.0)) * step(-40.0, front);
  col = col * shown + (uAcc * 1.4 + float3(0.45, 0.45, 0.45)) * edge * 0.9;
  float alpha = a * saturate(shown + edge);
  return float4(col * alpha, alpha);
}
"#;

struct Bytecode {
    vs: Vec<u8>,
    ps: Vec<u8>,
}

static BYTECODE: OnceLock<Option<Bytecode>> = OnceLock::new();

fn compile(entry: &[u8], target: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let r = unsafe {
        D3DCompile(
            HLSL.as_ptr().cast(),
            HLSL.len(),
            PCSTR(c"frame.hlsl".as_ptr().cast()),
            None,
            None,
            PCSTR(entry.as_ptr()),
            PCSTR(target.as_ptr()),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    let blob_bytes = |b: &ID3DBlob| unsafe {
        std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec()
    };
    match (r, code) {
        (Ok(()), Some(c)) => Ok(blob_bytes(&c)),
        (r, _) => Err(format!(
            "{:?}: {}",
            r.err(),
            errors.as_ref().map(|e| String::from_utf8_lossy(&blob_bytes(e)).into_owned()).unwrap_or_default()
        )),
    }
}

/// Скомпилировать шейдер рамки (один раз на процесс). false — компилятора нет или шейдер не
/// собрался: рамка тогда рисуется просто картинкой, как в вебе без WebGL2.
pub fn warm_up() -> bool {
    BYTECODE
        .get_or_init(|| match (compile(b"vs\0", b"vs_4_0\0"), compile(b"ps\0", b"ps_4_0\0")) {
            (Ok(vs), Ok(ps)) => Some(Bytecode { vs, ps }),
            (vs, ps) => {
                crate::native::log(&format!("main: frame shader failed: {:?} {:?}", vs.err(), ps.err()));
                None
            }
        })
        .is_some()
}

/// Картинки рамки в пикселях кадра: BGRA premultiplied, w×h.
pub struct FramePixels {
    pub w: u32,
    pub h: u32,
    pub color: Vec<u8>,
    pub normal: Vec<u8>,
}

pub struct FrameShader {
    generation: u64,
    size: (u32, u32),
    rtv: ID3D11RenderTargetView,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    color: ID3D11ShaderResourceView,
    normal: ID3D11ShaderResourceView,
    bitmap: ID2D1Bitmap1,
    _target: ID3D11Texture2D,
    last: Option<Vec<i32>>,
}

fn missing() -> windows::core::Error {
    windows::core::Error::from(E_FAIL)
}

fn texture(gpu: &GpuCx<'_>, w: u32, h: u32, px: &[u8]) -> Result<ID3D11ShaderResourceView> {
    unsafe {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let data = D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr().cast(), SysMemPitch: w * 4, SysMemSlicePitch: 0 };
        let mut tex = None;
        gpu.d3d.CreateTexture2D(&desc, Some(&data), Some(&mut tex))?;
        let tex = tex.ok_or_else(missing)?;
        let mut srv = None;
        gpu.d3d.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
        srv.ok_or_else(missing)
    }
}

impl FrameShader {
    /// Шейдер рамки под картинки `px` на устройстве `gpu`; None — шейдера нет.
    pub fn new(gpu: &GpuCx<'_>, px: &FramePixels) -> Result<Option<Self>> {
        if !warm_up() {
            return Ok(None);
        }
        let Some(code) = BYTECODE.get().and_then(|b| b.as_ref()) else { return Ok(None) };
        let (w, h) = (px.w.max(2), px.h.max(2));
        if px.color.len() != (w * h * 4) as usize || px.normal.len() != px.color.len() {
            return Ok(None);
        }
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut target = None;
            gpu.d3d.CreateTexture2D(&desc, None, Some(&mut target))?;
            let target = target.ok_or_else(missing)?;
            let mut rtv = None;
            gpu.d3d.CreateRenderTargetView(&target, None, Some(&mut rtv))?;
            let mut vs = None;
            gpu.d3d.CreateVertexShader(&code.vs, None, Some(&mut vs))?;
            let mut ps = None;
            gpu.d3d.CreatePixelShader(&code.ps, None, Some(&mut ps))?;
            let cb_desc = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<Constants>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let mut cb = None;
            gpu.d3d.CreateBuffer(&cb_desc, None, Some(&mut cb))?;
            let sampler_desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MipLODBias: 0.0,
                MaxAnisotropy: 1,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                BorderColor: [0.0; 4],
                MinLOD: 0.0,
                MaxLOD: f32::MAX,
            };
            let mut sampler = None;
            gpu.d3d.CreateSamplerState(&sampler_desc, Some(&mut sampler))?;
            let color = texture(gpu, w, h, &px.color)?;
            let normal = texture(gpu, w, h, &px.normal)?;
            let surface: IDXGISurface = target.cast()?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let bitmap = gpu.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            Ok(Some(Self {
                generation: gpu.generation,
                size: (w, h),
                rtv: rtv.ok_or_else(missing)?,
                vs: vs.ok_or_else(missing)?,
                ps: ps.ok_or_else(missing)?,
                cb: cb.ok_or_else(missing)?,
                sampler: sampler.ok_or_else(missing)?,
                color,
                normal,
                bitmap,
                _target: target,
                last: None,
            }))
        }
    }

    /// Шейдер годится для этого устройства и размера кадра.
    pub fn fits(&self, gpu: &GpuCx<'_>, size: (u32, u32)) -> bool {
        self.generation == gpu.generation && self.size == (size.0.max(2), size.1.max(2))
    }

    /// Перерисовать текстуру, если освещение изменилось. `skin` — размер скина в DIP.
    pub fn render(&mut self, gpu: &GpuCx<'_>, skin: (f32, f32), light: &FrameLight) {
        let key = light.key();
        if self.last.as_ref() == Some(&key) {
            return;
        }
        self.last = Some(key);
        let lk = |i: usize| [light.lunki[i][0], light.lunki[i][1], light.lunki[i][2], 0.0];
        let c = Constants {
            canvas: [self.size.0 as f32, self.size.1 as f32],
            skin: [skin.0, skin.1],
            mouse: [light.mouse.0, light.mouse.1],
            mouse_on: light.mouse_on,
            _p0: 0.0,
            orb: [light.orb.0, light.orb.1],
            orb_i: light.orb_light,
            sweep: light.sweep_x,
            sweep_i: light.sweep_i,
            reveal: light.reveal,
            _p1: [0.0; 2],
            lunki: [lk(0), lk(1), lk(2)],
        };
        let ctx = gpu.context;
        unsafe {
            ctx.ClearRenderTargetView(&self.rtv, &[0.0; 4]);
            ctx.UpdateSubresource(&self.cb, 0, None, &c as *const Constants as *const _, 0, 0);
            ctx.OMSetRenderTargets(Some(&[Some(self.rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: self.size.0 as f32,
                Height: self.size.1 as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.PSSetShaderResources(0, Some(&[Some(self.color.clone()), Some(self.normal.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.RSSetState(None);
            ctx.OMSetBlendState(None, None, u32::MAX);
            ctx.Draw(3, 0);
            ctx.PSSetShaderResources(0, Some(&[None, None]));
            ctx.OMSetRenderTargets(None, None);
        }
    }

    pub fn bitmap(&self) -> &ID2D1Bitmap1 {
        &self.bitmap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_shader_compiles() {
        assert!(warm_up(), "frame.hlsl must compile with d3dcompiler_47");
    }

    #[test]
    fn light_key_ignores_sub_thousandth_changes() {
        let base = FrameLight {
            mouse: (0.0, 0.0),
            mouse_on: 0.0,
            orb: (310.0, 352.0),
            orb_light: 0.35,
            sweep_x: -1.0,
            sweep_i: 0.0,
            reveal: 1800.0,
            lunki: [[965.0, 100.0, 0.0], [1015.0, 100.0, 0.0], [1065.0, 100.0, 0.0]],
        };
        let mut nudged = base;
        nudged.orb_light += 0.0001;
        assert_eq!(base.key(), nudged.key());
        nudged.orb_light += 0.01;
        assert_ne!(base.key(), nudged.key());
    }
}
