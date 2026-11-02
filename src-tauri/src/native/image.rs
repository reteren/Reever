// Картинки нативных окон: декодирование WebP/PNG через WIC и кэш битмапов под текущий масштаб.
//
// Источник хранится сжатым (путь к файлу или байты), а в видеопамять идёт копия ровно того
// размера в пикселях, в котором её рисуют: кадр маскота 384×512 при окне 160×213 DIP и масштабе
// 150 % декодируется сразу в 240×320 — рисуется пиксель в пиксель, без пересэмплирования на
// каждом кадре, и весит вчетверо меньше. Полноразмерные декодированные кадры не держатся:
// 296 кадров маскота по 384×512 заняли бы 230 МБ. Кэш ограничен по байтам и вытесняет давно
// не рисованное; декодирование одного кадра — 1–2 мс, так что промах кэша на анимации не страшен.
// При потере устройства кэш сбрасывается целиком (битмапы принадлежат старому устройству),
// источники остаются.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use windows::core::{Result, HSTRING};
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, ID2D1DeviceContext, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory2, GUID_ContainerFormatPng, GUID_ContainerFormatWebp,
    GUID_WICPixelFormat32bppPBGRA, IWICBitmapDecoder,
    IWICBitmapSource, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapInterpolationModeHighQualityCubic, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

/// Номер картинки в реестре `Images`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageId(pub(crate) u32);

#[derive(Clone)]
enum Source {
    File(PathBuf),
    Bytes(Arc<[u8]>),
}

struct SourceEntry {
    source: Source,
    /// Натуральный размер, узнаётся при первом обращении.
    size: Option<(u32, u32)>,
}

struct Cached {
    bitmap: ID2D1Bitmap1,
    bytes: usize,
    last_used: u64,
}

/// Бюджет кэша по умолчанию: ~48 МБ видеопамяти — весь цикл кадров маскота при 100–150 %.
pub const DEFAULT_CACHE_BUDGET: usize = 48 * 1024 * 1024;

pub struct Images {
    wic: IWICImagingFactory,
    sources: Vec<SourceEntry>,
    cache: HashMap<(u32, u32, u32), Cached>,
    used_bytes: usize,
    budget: usize,
    tick: u64,
    decodes: u64,
}

impl Images {
    /// Вызывать в потоке с инициализированным COM.
    pub fn new() -> Result<Self> {
        let wic = unsafe { CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER)? };
        Ok(Self {
            wic,
            sources: Vec::new(),
            cache: HashMap::new(),
            used_bytes: 0,
            budget: DEFAULT_CACHE_BUDGET,
            tick: 0,
            decodes: 0,
        })
    }

    /// Загрузить кодеки WebP и PNG заранее: первое декодирование WebP иначе тратит десятки
    /// миллисекунд на загрузку кодека прямо в первом кадре окна.
    pub fn prewarm_codecs(&self) {
        for format in [GUID_ContainerFormatWebp, GUID_ContainerFormatPng] {
            let _ = unsafe { self.wic.CreateDecoder(&format, std::ptr::null()) };
        }
    }

    /// Картинка из файла; файл читается при каждом декодировании (держит в памяти только кэш ОС).
    pub fn add_file(&mut self, path: impl Into<PathBuf>) -> ImageId {
        self.push(Source::File(path.into()))
    }

    /// Картинка из байт (например, include_bytes!).
    pub fn add_bytes(&mut self, bytes: impl Into<Arc<[u8]>>) -> ImageId {
        self.push(Source::Bytes(bytes.into()))
    }

    fn push(&mut self, source: Source) -> ImageId {
        self.sources.push(SourceEntry { source, size: None });
        ImageId(self.sources.len() as u32 - 1)
    }

    /// Натуральный размер картинки в пикселях (читается заголовок, без полного декодирования).
    pub fn size(&mut self, id: ImageId) -> Result<(u32, u32)> {
        let entry = self.entry(id)?;
        if let Some(s) = entry.size {
            return Ok(s);
        }
        let source = entry.source.clone();
        let decoder = self.decoder(&source)?;
        let mut w = 0;
        let mut h = 0;
        unsafe { decoder.GetFrame(0)?.GetSize(&mut w, &mut h)? };
        self.sources[id.0 as usize].size = Some((w, h));
        Ok((w, h))
    }

    /// Бюджет кэша в байтах; лишнее вытесняется сразу.
    pub fn set_budget(&mut self, bytes: usize) {
        self.budget = bytes;
        self.evict(0);
    }

    /// (битмапов в кэше, байт в кэше, декодирований всего).
    pub fn stats(&self) -> (usize, usize, u64) {
        (self.cache.len(), self.used_bytes, self.decodes)
    }

    /// Устройство потеряно или сменилось: все битмапы устарели.
    pub fn clear_device(&mut self) {
        self.cache.clear();
        self.used_bytes = 0;
    }

    /// Битмап картинки ровно `w`×`h` пикселей (из кэша или свежедекодированный).
    pub fn bitmap(&mut self, dc: &ID2D1DeviceContext, id: ImageId, w: u32, h: u32) -> Result<ID2D1Bitmap1> {
        let (w, h) = (w.max(1), h.max(1));
        self.tick += 1;
        let key = (id.0, w, h);
        if let Some(c) = self.cache.get_mut(&key) {
            c.last_used = self.tick;
            return Ok(c.bitmap.clone());
        }
        let source = self.entry(id)?.source.clone();
        let bitmap = self.decode(dc, &source, w, h)?;
        self.decodes += 1;
        let bytes = w as usize * h as usize * 4;
        self.evict(bytes);
        self.used_bytes += bytes;
        self.cache.insert(key, Cached { bitmap: bitmap.clone(), bytes, last_used: self.tick });
        Ok(bitmap)
    }

    fn entry(&self, id: ImageId) -> Result<&SourceEntry> {
        self.sources
            .get(id.0 as usize)
            .ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_INVALIDARG))
    }

    /// Освободить место под `incoming` байт, выкидывая давно не рисованное.
    fn evict(&mut self, incoming: usize) {
        while self.used_bytes + incoming > self.budget && !self.cache.is_empty() {
            let oldest = self.cache.iter().min_by_key(|(_, c)| c.last_used).map(|(k, _)| *k);
            if let Some(k) = oldest {
                if let Some(c) = self.cache.remove(&k) {
                    self.used_bytes -= c.bytes;
                }
            }
        }
    }

    fn decoder(&self, source: &Source) -> Result<IWICBitmapDecoder> {
        unsafe {
            match source {
                Source::File(path) => self.wic.CreateDecoderFromFilename(
                    &HSTRING::from(path.as_os_str()),
                    None,
                    GENERIC_READ,
                    WICDecodeMetadataCacheOnDemand,
                ),
                Source::Bytes(bytes) => {
                    let stream = self.wic.CreateStream()?;
                    stream.InitializeFromMemory(bytes)?;
                    self.wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
                }
            }
        }
    }

    fn decode(&self, dc: &ID2D1DeviceContext, source: &Source, w: u32, h: u32) -> Result<ID2D1Bitmap1> {
        unsafe {
            let frame = self.decoder(source)?.GetFrame(0)?;
            // сначала в premultiplied BGRA, потом масштаб: уменьшение в premultiplied не даёт
            // тёмной каймы на краях полупрозрачного силуэта
            let converter = self.wic.CreateFormatConverter()?;
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            let (mut fw, mut fh) = (0, 0);
            frame.GetSize(&mut fw, &mut fh)?;
            let src: IWICBitmapSource = if (fw, fh) == (w, h) {
                converter.into()
            } else {
                let scaler = self.wic.CreateBitmapScaler()?;
                scaler.Initialize(&converter, w, h, WICBitmapInterpolationModeHighQualityCubic)?;
                scaler.into()
            };
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            dc.CreateBitmapFromWicBitmap(&src, Some(&props))
        }
    }
}
