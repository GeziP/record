//! 音频端点枚举：默认设备（两个角色）、全部活动设备、设备名与混音格式、端点电平表。

use crate::com::ComGuard;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{DEVICE_STATE_ACTIVE, EDataFlow, ERole, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eCommunications, eConsole, eRender};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::core::{Interface, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Render,
    Capture,
}

impl Flow {
    fn raw(self) -> EDataFlow {
        match self {
            Flow::Render => eRender,
            Flow::Capture => eCapture,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Console,
    Communications,
}

impl Role {
    fn raw(self) -> ERole {
        match self {
            Role::Console => eConsole,
            Role::Communications => eCommunications,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MixFormat {
    pub rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub float: bool,
}

pub fn enumerator() -> Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

pub fn default_endpoint(e: &IMMDeviceEnumerator, flow: Flow, role: Role) -> Result<IMMDevice> {
    unsafe { e.GetDefaultAudioEndpoint(flow.raw(), role.raw()) }
}

pub fn endpoint_by_id(e: &IMMDeviceEnumerator, id: &str) -> Result<IMMDevice> {
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { e.GetDevice(windows::core::PCWSTR(wide.as_ptr())) }
}

pub fn device_id(d: &IMMDevice) -> Result<String> {
    unsafe {
        let p = d.GetId()?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        Ok(s)
    }
}

pub fn device_name(d: &IMMDevice) -> Result<String> {
    unsafe {
        let store = d.OpenPropertyStore(STGM_READ)?;
        let v = store.GetValue(&PKEY_Device_FriendlyName)?;
        let p = PropVariantToStringAlloc(&v)?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        Ok(s)
    }
}

pub fn info(d: &IMMDevice) -> Result<EndpointInfo> {
    Ok(EndpointInfo { id: device_id(d)?, name: device_name(d).unwrap_or_else(|_| "（未知设备）".into()) })
}

/// 全部处于活动状态的端点。
pub fn active_endpoints(e: &IMMDeviceEnumerator, flow: Flow) -> Result<Vec<IMMDevice>> {
    unsafe {
        let col = e.EnumAudioEndpoints(flow.raw(), DEVICE_STATE_ACTIVE)?;
        let n = col.GetCount()?;
        (0..n).map(|i| col.Item(i)).collect()
    }
}

/// 设备的共享模式混音格式。
pub fn mix_format(d: &IMMDevice) -> Result<MixFormat> {
    unsafe {
        let client: IAudioClient = d.Activate(CLSCTX_ALL, None)?;
        let p = client.GetMixFormat()?;
        let f = &*p;
        // WAVE_FORMAT_IEEE_FLOAT = 3；扩展格式（0xFFFE）的子格式这里不展开，位深 32 视为浮点
        let float = f.wFormatTag == 3 || (f.wFormatTag == 0xFFFE && f.wBitsPerSample == 32);
        let m = MixFormat { rate: f.nSamplesPerSec, channels: f.nChannels, bits: f.wBitsPerSample, float };
        CoTaskMemFree(Some(p as *const _));
        Ok(m)
    }
}

/// 端点电平表的当前峰值（0.0–1.0），静默失效监测用（设计文档 5.10）。
pub struct Meter {
    meter: IAudioMeterInformation,
}

impl Meter {
    pub fn open(d: &IMMDevice) -> Result<Self> {
        let meter: IAudioMeterInformation = unsafe { d.Activate(CLSCTX_ALL, None)? };
        Ok(Self { meter })
    }

    pub fn peak(&self) -> f32 {
        unsafe { self.meter.GetPeakValue().unwrap_or(0.0) }
    }
}

/// 所有活动播放设备电平表的最大峰值。取不到任何电平表时返回 `None`（不报告，也不猜）。
pub struct RenderMeters {
    meters: Vec<(String, Meter)>,
}

impl RenderMeters {
    pub fn open_all(e: &IMMDeviceEnumerator) -> Result<Self> {
        let mut meters = Vec::new();
        for d in active_endpoints(e, Flow::Render)? {
            if let (Ok(i), Ok(m)) = (info(&d), Meter::open(&d)) {
                meters.push((i.name, m));
            }
        }
        Ok(Self { meters })
    }

    pub fn max_peak(&self) -> Option<f32> {
        self.meters.iter().map(|(_, m)| m.peak()).fold(None, |acc, p| Some(acc.map_or(p, |a: f32| a.max(p))))
    }

    /// 峰值最高的设备名，告警时指明哪个设备有声。
    pub fn loudest(&self) -> Option<(String, f32)> {
        self.meters.iter().map(|(n, m)| (n.clone(), m.peak())).max_by(|a, b| a.1.total_cmp(&b.1))
    }
}

/// 设置界面的设备下拉框用的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEntry {
    pub id: String,
    pub name: String,
    pub flow: Flow,
    /// 是不是这个方向上“默认设备”（Console 角色）。
    pub is_default: bool,
}

/// 全部活动的播放与录音设备。COM 在自己的线程里初始化，所以可以在任何线程调用（包括已经是单线程套间的界面线程）。
pub fn list_active() -> std::result::Result<Vec<DeviceEntry>, String> {
    std::thread::spawn(|| -> Result<Vec<DeviceEntry>> {
        let _com = ComGuard::init()?;
        let e = enumerator()?;
        let mut out = Vec::new();
        for flow in [Flow::Render, Flow::Capture] {
            let default_id = default_endpoint(&e, flow, Role::Console).ok().and_then(|d| device_id(&d).ok());
            for d in active_endpoints(&e, flow)? {
                let i = info(&d)?;
                out.push(DeviceEntry { is_default: default_id.as_deref() == Some(i.id.as_str()), id: i.id, name: i.name, flow });
            }
        }
        Ok(out)
    })
    .join()
    .map_err(|_| "枚举设备的线程崩溃了".to_string())?
    .map_err(|e| e.to_string())
}

/// 把 `Interface` 的 QueryInterface 失败也带上设备名，诊断时有用。
pub fn describe<T: Interface>(_: &T) -> &'static str {
    std::any::type_name::<T>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_active_devices_can_be_listed_from_any_thread() {
        // 本机没有音频设备时列表为空也算正常；有设备时每个方向至多一个默认设备
        let list = list_active().expect("枚举设备");
        for flow in [Flow::Render, Flow::Capture] {
            let defaults = list.iter().filter(|d| d.flow == flow && d.is_default).count();
            assert!(defaults <= 1, "{flow:?} 有 {defaults} 个默认设备");
        }
        assert!(list.iter().all(|d| !d.id.is_empty() && !d.name.is_empty()));
    }
}
