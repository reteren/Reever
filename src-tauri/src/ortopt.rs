//! Заранее оптимизированные графы моделей: быстрее загрузка большой модели.
//!
//! ONNX Runtime при каждой загрузке заново оптимизирует граф. Оптимизированный один раз
//! граф (ORT_ENABLE_EXTENDED — переносим между процессорами) грузится быстрее: Parakeet v3 —
//! 1,13 с вместо 1,5–1,85 с при той же скорости распознавания (tools/kws/load_bench.py).
//! sherpa-onnx сохранить граф не умеет, поэтому его сохраняет сам Reever через C API
//! ONNX Runtime, вшитого в sherpa-onnx, — в отдельном скрытом процессе
//! (`reever.exe --optimize-model <папка>`): сессия на ~700 МБ освобождается вместе с ним.
//!
//! Кэш — `%LOCALAPPDATA%\com.troid.reever\opt\<версия ORT>\<папка модели>`; новая версия
//! ORT — новый кэш. Не загрузился граф — asr.rs берёт исходные файлы.

use std::ffi::{c_char, c_void, CStr};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context};

/// Готовый кэш папки модели: все графы сохранены.
const DONE: &str = ".done";
/// Подготовка уже идёт (распознаватель перезапускается при сохранении настроек).
static PREPARING: AtomicBool = AtomicBool::new(false);

#[repr(C)]
struct OrtApiBase {
    get_api: unsafe extern "system" fn(u32) -> *const *const c_void,
    get_version_string: unsafe extern "system" fn() -> *const c_char,
}

#[link(name = "onnxruntime", kind = "static")]
extern "C" {
    fn OrtGetApiBase() -> *const OrtApiBase;
}

/// Версия ONNX Runtime, вшитого в sherpa-onnx.
pub fn ort_version() -> String {
    unsafe { CStr::from_ptr(((*OrtGetApiBase()).get_version_string)()) }
        .to_string_lossy()
        .into_owned()
}

fn cache_root() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|dir| {
        PathBuf::from(dir)
            .join("com.troid.reever")
            .join("opt")
            .join(ort_version())
    })
}

fn cache_dir(model_dir: &Path) -> Option<PathBuf> {
    Some(cache_root()?.join(model_dir.file_name()?))
}

/// Отпечаток исходного файла: размер и время изменения. Подменили модель в папке —
/// отпечаток другой, и старый граф не используется.
fn fingerprint(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(format!("{}\t{}\t{modified}", path.file_name()?.to_string_lossy(), meta.len()))
}

/// Кэш папки готов и сделан из тех же файлов, что лежат в ней сейчас.
fn fresh(model_dir: &Path) -> bool {
    let Some(done) = cache_dir(model_dir).and_then(|dir| std::fs::read_to_string(dir.join(DONE)).ok()) else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(model_dir) else { return false };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "onnx"))
        .all(|path| fingerprint(&path).is_some_and(|line| done.lines().any(|known| known == line)))
}

/// Оптимизированный граф для файла модели, если кэш папки готов и свежий.
pub fn cached(model_file: &Path) -> Option<PathBuf> {
    let model_dir = model_file.parent()?;
    let file = cache_dir(model_dir)?.join(model_file.file_name()?);
    (fresh(model_dir) && file.is_file()).then_some(file)
}

/// Забыть кэш папки модели (граф не загрузился).
pub fn forget(model_dir: &Path) {
    if let Some(dir) = cache_dir(model_dir) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Подготовить кэш для папок моделей, где его ещё нет, — по одной, в скрытом процессе
/// с пониженным приоритетом. Ждёт завершения; вызывать из фонового потока.
pub fn prepare(model_dirs: &[PathBuf]) {
    let Ok(exe) = std::env::current_exe() else { return };
    if PREPARING.swap(true, Ordering::SeqCst) {
        return;
    }
    for dir in model_dirs {
        if fresh(dir) || !dir.is_dir() {
            continue;
        }
        // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS
        let _ = std::process::Command::new(&exe)
            .arg("--optimize-model")
            .arg(dir)
            .creation_flags(0x0800_0000 | 0x0000_4000)
            .status();
    }
    PREPARING.store(false, Ordering::SeqCst);
}

/// Тело `reever.exe --optimize-model <папка>`: сохранить оптимизированные графы всех
/// .onnx папки в кэш. Процесс сразу после этого завершается.
pub fn run_cli(model_dir: &Path) -> anyhow::Result<()> {
    let target = cache_dir(model_dir).context("no LOCALAPPDATA")?;
    let mut partial_name = target.file_name().context("model folder name")?.to_os_string();
    partial_name.push(".partial");
    let partial = target.with_file_name(partial_name);
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial)?;
    let mut prints = Vec::new();
    for entry in std::fs::read_dir(model_dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "onnx") {
            optimize_file(&path, &partial.join(path.file_name().unwrap()))
                .with_context(|| format!("optimizing {}", path.display()))?;
            prints.extend(fingerprint(&path));
        }
    }
    std::fs::write(partial.join(DONE), prints.join("\n"))?;
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&partial, &target)?;
    Ok(())
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// Создаёт сессию ORT с `SetOptimizedModelFilePath`: ORT сохраняет граф после оптимизации.
/// Ресурсы не освобождаются — процесс завершится сразу после работы.
fn optimize_file(input: &Path, output: &Path) -> anyhow::Result<()> {
    type Status = *mut c_void;
    unsafe {
        let api = ((*OrtGetApiBase()).get_api)(17);
        if api.is_null() {
            bail!("ONNX Runtime C API 17 is not available");
        }
        // Номера функций в таблице OrtApi (стабильны, onnxruntime_c_api.h).
        let f = |index: usize| *api.add(index);
        let error: unsafe extern "system" fn(Status) -> *const c_char = std::mem::transmute(f(2));
        let check = |status: Status| -> anyhow::Result<()> {
            if status.is_null() {
                Ok(())
            } else {
                bail!("{}", CStr::from_ptr(error(status)).to_string_lossy())
            }
        };
        let create_env: unsafe extern "system" fn(i32, *const c_char, *mut *mut c_void) -> Status =
            std::mem::transmute(f(3));
        let create_session: unsafe extern "system" fn(*mut c_void, *const u16, *mut c_void, *mut *mut c_void) -> Status =
            std::mem::transmute(f(7));
        let create_options: unsafe extern "system" fn(*mut *mut c_void) -> Status = std::mem::transmute(f(10));
        let set_optimized_path: unsafe extern "system" fn(*mut c_void, *const u16) -> Status =
            std::mem::transmute(f(11));
        let set_level: unsafe extern "system" fn(*mut c_void, i32) -> Status = std::mem::transmute(f(23));

        let mut env = std::ptr::null_mut();
        check(create_env(3, c"reever-opt".as_ptr(), &mut env))?;
        let mut options = std::ptr::null_mut();
        check(create_options(&mut options))?;
        check(set_level(options, 2))?; // ORT_ENABLE_EXTENDED
        let output = wide(output);
        check(set_optimized_path(options, output.as_ptr()))?;
        let input = wide(input);
        let mut session = std::ptr::null_mut();
        check(create_session(env, input.as_ptr(), options, &mut session))
    }
}
