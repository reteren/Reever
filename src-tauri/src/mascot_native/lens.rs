// Жидкая линза в глазу нативного маскота: тот же фрагментный шейдер, что у skin/orb.js
// (createCore), переписанный на HLSL. Рисуется пиксельным шейдером D3D11 в свою маленькую
// текстуру перед кадром (WindowHandler::prepare), а в кадре текстура кладётся на купол глаза
// как битмап Direct2D — так же, как веб-маскот кладёт холст линзы через drawImage.
//
// Шейдер компилируется при запуске (d3dcompiler_47.dll есть в Windows 10+), один раз на процесс:
// `warm_up` зовут заранее, чтобы первый показ маскота не ждал компилятора.

use std::sync::OnceLock;

use windows::core::{Interface, Result, PCSTR};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap1, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Buffer, ID3D11PixelShader, ID3D11RenderTargetView, ID3D11Texture2D, ID3D11VertexShader,
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BUFFER_DESC,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGISurface;

use crate::native::GpuCx;

/// Параметры кадра линзы — как `render(s)` в orb.js.
#[derive(Debug, Clone, Copy, Default)]
pub struct LensParams {
    pub time: f32,
    pub fill: f32,
    pub wave: f32,
    pub energy: f32,
    pub swirl: f32,
    pub flash: f32,
    pub dark: f32,
    pub glitch: f32,
}

/// Константный буфер: порядок и выравнивание по 16 байт — как `cbuffer P` в HLSL.
#[repr(C)]
#[derive(Clone, Copy)]
struct Constants {
    res: [f32; 2],
    time: f32,
    level: f32,
    wave: f32,
    energy: f32,
    swirl: f32,
    flash: f32,
    dark: f32,
    glitch: f32,
    _pad: [f32; 2],
}

const HLSL: &str = r#"
cbuffer P : register(b0) {
  float2 uRes; float uTime; float uLevel;
  float uWave; float uEnergy; float uSwirl; float uFlash;
  float uDark; float uGlitch; float2 uPad;
};
static const float3 uAcc = float3(39.0, 230.0, 170.0) / 255.0;

// треугольник на весь экран; обход по часовой — лицевой для растеризатора по умолчанию
float4 vs(uint id : SV_VertexID) : SV_Position {
  float2 p = id == 0 ? float2(-1, -1) : id == 1 ? float2(-1, 3) : float2(3, -1);
  return float4(p, 0, 1);
}

float hash(float n) { return frac(sin(n) * 43758.5453); }
float hash2(float2 p) { return frac(sin(dot(p, float2(127.1, 311.7))) * 43758.5453); }
float noise(float2 p) {
  float2 i = floor(p), f = frac(p); f = f * f * (3.0 - 2.0 * f);
  return lerp(lerp(hash2(i), hash2(i + float2(1, 0)), f.x), lerp(hash2(i + float2(0, 1)), hash2(i + float2(1, 1)), f.x), f.y);
}
float fbm(float2 p) { float s = 0.0, a = 0.5; [unroll] for (int i = 0; i < 5; i++) { s += a * noise(p); p = p * 2.03 + 7.1; a *= 0.5; } return s; }
float gmod(float x, float y) { return x - y * floor(x / y); }

float4 ps(float4 pos : SV_Position) : SV_Target {
  // gl_FragCoord отсчитывается снизу, SV_Position — сверху
  float2 q = float2(pos.x / uRes.x, 1.0 - pos.y / uRes.y) * 2.0 - 1.0;
  q /= 0.965;
  if (uGlitch > 0.0) {
    float band = floor(q.y * 16.0 + floor(uTime * 24.0));
    float on = step(0.72, hash(band * 1.37 + floor(uTime * 13.0)));
    q.x += (hash(band + floor(uTime * 21.0)) - 0.5) * 0.22 * uGlitch * on;
  }
  float r = length(q);
  if (r > 1.0) return float4(0, 0, 0, 0);
  float z = sqrt(max(0.0, 1.0 - r * r));
  float2 qi = q * (0.74 + 0.26 * z);

  float ang = uSwirl * (uTime * 1.6 + (1.2 - r) * 2.4);
  float ca = cos(ang), sa = sin(ang);
  // mat2(c, -s, s, c) * v в GLSL (по столбцам)
  float2 qs = float2(ca * qi.x + sa * qi.y, -sa * qi.x + ca * qi.y);

  float surf = -1.0 + 2.0 * uLevel
    + uWave * (0.075 * sin(qi.x * 5.5 + uTime * 3.1) + 0.045 * sin(qi.x * 11.0 - uTime * 4.7) + 0.02 * sin(qi.x * 23.0 + uTime * 8.3))
    + 0.025 * sin(uTime * 0.9 + qi.x * 1.3);
  float inLiq = smoothstep(surf + 0.014, surf - 0.014, qi.y);
  float depth = clamp(surf - qi.y, 0.0, 2.0);

  float caus = fbm(qs * 2.7 + float2(0.0, -uTime * 0.38));
  float caus2 = fbm(qs * 6.0 - float2(uTime * 0.2, uTime * 0.55));
  float3 liq = uAcc * (0.38 + 1.05 * caus + 0.35 * caus2) * (1.0 - 0.42 * depth);
  liq += uAcc * exp(-depth * 5.5) * 0.9 + float3(0.55, 1.0, 0.85) * exp(-depth * 14.0) * 0.35;

  float bub = 0.0;
  [unroll] for (int i = 0; i < 9; i++) {
    float fi = float(i);
    float bx = (hash(fi * 3.1) - 0.5) * 1.3 + 0.04 * sin(uTime * 2.0 + fi);
    float by = -1.0 + gmod(uTime * (0.18 + 0.22 * hash(fi * 7.7)) + hash(fi * 5.3) * 2.0, 2.0);
    float d = length(qi - float2(bx, by));
    float rr = 0.018 + 0.02 * hash(fi * 9.1);
    bub += smoothstep(rr, rr * 0.4, d) * step(by, surf);
  }
  liq += float3(0.7, 1.0, 0.9) * bub * 0.5;

  float m = (qi.y - surf) / 0.02;
  float men = exp(-m * m);
  float3 empty = float3(0.003, 0.016, 0.013) + uAcc * 0.12 * exp(-max(qi.y - surf, 0.0) * 3.2) * uEnergy;
  float3 col = lerp(empty, liq * uEnergy, inLiq);
  col += (uAcc * 0.8 + 0.45) * men * (0.35 + uEnergy) * 0.75;
  col += uAcc * 0.22 * uEnergy * exp(-r * r * 2.2) * inLiq;

  float fres = pow(1.0 - z, 2.4);
  col *= lerp(1.0, 0.4, fres);
  col += float3(0.7, 1.0, 0.92) * fres * 0.22 + uAcc * fres * 0.3 * uEnergy;
  float2 hp = (q - float2(-0.3, 0.44)) * float2(1.0, 1.75);
  col += float3(0.85, 1.0, 0.95) * smoothstep(0.44, 0.1, length(hp)) * 0.28 * (1.0 - uDark * 0.5);
  col += 1.0 * smoothstep(0.075, 0.015, length(q - float2(-0.42, 0.52))) * 0.95;
  col += uAcc * smoothstep(0.26, 0.0, length((q - float2(0.46, -0.56)) * float2(1.0, 2.1))) * 0.22 * uEnergy;

  col += (uAcc + 0.65) * uFlash * (1.0 - r * 0.45);
  col *= lerp(1.0, 0.3, uDark);
  float alpha = smoothstep(1.0, 0.985, r);
  // premultiplied, как у WebGL-холста с premultipliedAlpha; цвет выше 1 срезается так же
  return float4(saturate(col) * alpha, alpha);
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
            PCSTR(c"lens.hlsl".as_ptr().cast()),
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

/// Скомпилировать шейдеры линзы (один раз на процесс). None — компилятор недоступен или
/// шейдер не собрался; маскот тогда рисуется без жидкости в глазу.
pub fn warm_up() -> bool {
    BYTECODE
        .get_or_init(|| match (compile(b"vs\0", b"vs_4_0\0"), compile(b"ps\0", b"ps_4_0\0")) {
            (Ok(vs), Ok(ps)) => Some(Bytecode { vs, ps }),
            (vs, ps) => {
                eprintln!("[reever] mascot lens shader: {:?} {:?}", vs.err(), ps.err());
                None
            }
        })
        .is_some()
}

pub struct Lens {
    generation: u64,
    size: u32,
    rtv: ID3D11RenderTargetView,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    bitmap: ID2D1Bitmap1,
    _texture: ID3D11Texture2D,
}

impl Lens {
    /// Линза размером `size`×`size` пикселей на устройстве `gpu`; None — шейдера нет.
    pub fn new(gpu: &GpuCx<'_>, size: u32) -> Result<Option<Self>> {
        if !warm_up() {
            return Ok(None);
        }
        let Some(code) = BYTECODE.get().and_then(|b| b.as_ref()) else { return Ok(None) };
        let size = size.max(2);
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: size,
                Height: size,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut texture = None;
            gpu.d3d.CreateTexture2D(&desc, None, Some(&mut texture))?;
            let texture = texture.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
            let mut rtv = None;
            gpu.d3d.CreateRenderTargetView(&texture, None, Some(&mut rtv))?;
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
            let surface: IDXGISurface = texture.cast()?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let bitmap = gpu.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            let missing = || windows::core::Error::from(windows::Win32::Foundation::E_FAIL);
            Ok(Some(Self {
                generation: gpu.generation,
                size,
                rtv: rtv.ok_or_else(missing)?,
                vs: vs.ok_or_else(missing)?,
                ps: ps.ok_or_else(missing)?,
                cb: cb.ok_or_else(missing)?,
                bitmap,
                _texture: texture,
            }))
        }
    }

    /// Линза годится для этого устройства и размера.
    pub fn fits(&self, gpu: &GpuCx<'_>, size: u32) -> bool {
        self.generation == gpu.generation && self.size == size.max(2)
    }

    pub fn render(&self, gpu: &GpuCx<'_>, p: &LensParams) {
        let c = Constants {
            res: [self.size as f32, self.size as f32],
            time: p.time,
            level: p.fill,
            wave: p.wave,
            energy: p.energy,
            swirl: p.swirl,
            flash: p.flash,
            dark: p.dark,
            glitch: p.glitch,
            _pad: [0.0; 2],
        };
        let ctx = gpu.context;
        unsafe {
            ctx.UpdateSubresource(&self.cb, 0, None, &c as *const Constants as *const _, 0, 0);
            ctx.OMSetRenderTargets(Some(&[Some(self.rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: self.size as f32,
                Height: self.size as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.RSSetState(None);
            ctx.OMSetBlendState(None, None, u32::MAX);
            ctx.Draw(3, 0);
            ctx.OMSetRenderTargets(None, None);
        }
    }

    pub fn bitmap(&self) -> &ID2D1Bitmap1 {
        &self.bitmap
    }
}
