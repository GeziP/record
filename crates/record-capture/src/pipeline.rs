//! 一块屏的采集管线：D3D11 设备 + Desktop Duplication + D3D11 视频处理器（设计文档 5.2）。
//!
//! 缩放、黑边与 BGRA→NV12 都在 GPU 上由视频处理器完成，读回 CPU 的只有 NV12（1080p 约 3 MB/帧）。
//! 4K 的 BGRA 帧有 33 MB，15 fps 下读回约 500 MB/s，所以 GPU 路径是必须而不是优化。
//!
//! 线程：整条管线只能在创建它的线程上使用（D3D11 立即上下文不是线程安全的）。

use crate::monitor::{MonitorInfo, sdr_white_nits};
use crate::tonemap::Tonemap;
use record_core::geometry::{Rect, letterbox, nv12_frame_bytes};
use std::fmt;
use std::sync::Arc;
use windows::Win32::Foundation::{E_ACCESSDENIED, HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
    D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11_VIDEO_COLOR, D3D11_VIDEO_COLOR_0, D3D11_VIDEO_COLOR_YCbCrA,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext,
    ID3D11VideoContext1, ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_MODE_ROTATION_IDENTITY,
    DXGI_MODE_ROTATION_UNSPECIFIED, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_HUNG, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_NOT_CURRENTLY_AVAILABLE, DXGI_ERROR_SESSION_DISCONNECTED,
    DXGI_ERROR_UNSUPPORTED, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_DESC, DXGI_OUTDUPL_FRAME_INFO, IDXGIFactory1, IDXGIOutput1, IDXGIOutput5, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 复制对象失效（显示模式变化、全屏独占切换等），需要重建。
    AccessLost,
    /// 系统不允许抓取：UAC、锁屏等安全桌面，或会话已断开。
    AccessDenied,
    /// 显卡设备丢失或重置，整套管线要重建。
    DeviceLost,
    /// 桌面复制名额被占满（同一会话数量有限），或此刻不可用。
    Busy,
    /// 这块屏的当前配置不支持（旋转、没有视频处理器、复制在系统内存里等）。
    Unsupported,
    Other,
}

#[derive(Debug, Clone)]
pub struct PipelineError {
    pub kind: ErrorKind,
    pub message: String,
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for PipelineError {}

fn classify(e: &windows::core::Error) -> ErrorKind {
    let c = e.code();
    if c == DXGI_ERROR_ACCESS_LOST {
        ErrorKind::AccessLost
    } else if c == E_ACCESSDENIED || c == DXGI_ERROR_SESSION_DISCONNECTED {
        ErrorKind::AccessDenied
    } else if c == DXGI_ERROR_DEVICE_REMOVED || c == DXGI_ERROR_DEVICE_RESET || c == DXGI_ERROR_DEVICE_HUNG {
        ErrorKind::DeviceLost
    } else if c == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE {
        ErrorKind::Busy
    } else if c == DXGI_ERROR_UNSUPPORTED {
        ErrorKind::Unsupported
    } else {
        ErrorKind::Other
    }
}

pub(crate) fn win(context: &str) -> impl Fn(windows::core::Error) -> PipelineError + '_ {
    move |e| PipelineError { kind: classify(&e), message: format!("{context}：{e}") }
}

pub(crate) fn fail(kind: ErrorKind, message: impl Into<String>) -> PipelineError {
    PipelineError { kind, message: message.into() }
}

pub struct Pipeline {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    /// 桌面是 FP16（HDR 开启）时的 HDR→SDR 转换。
    tonemap: Option<Tonemap>,
    vctx: ID3D11VideoContext1,
    vdev: ID3D11VideoDevice,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    nv12: ID3D11Texture2D,
    out_view: ID3D11VideoProcessorOutputView,
    staging: ID3D11Texture2D,
    /// 桌面复制给出的画面尺寸。
    pub src_w: u32,
    pub src_h: u32,
    pub out_w: u32,
    pub out_h: u32,
    /// 缩放后的画面在输出中的位置，其余是黑边。
    pub layout: Rect,
    /// 桌面复制给出的纹理格式（`DXGI_FORMAT` 的数值），第一次取到帧后才知道。
    pub src_format: Option<i32>,
}

fn rect(r: Rect) -> RECT {
    RECT { left: r.x as i32, top: r.y as i32, right: r.right() as i32, bottom: r.bottom() as i32 }
}

fn tex_desc(w: u32, h: u32) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    }
}

impl Pipeline {
    /// 为 `mon` 建一条输出 `out_w`×`out_h`（偶数）的管线。
    pub fn open(mon: &MonitorInfo, out_w: u32, out_h: u32) -> Result<Self, PipelineError> {
        assert!(out_w >= 2 && out_h >= 2 && out_w.is_multiple_of(2) && out_h.is_multiple_of(2), "输出尺寸必须是不小于 2 的偶数");
        if mon.rotation > 1 {
            return Err(fail(ErrorKind::Unsupported, format!("显示器有旋转（代码 {}），暂不支持", mon.rotation)));
        }

        // 设备必须建在输出所在的显卡上，跨显卡的桌面复制不被支持
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(win("创建 DXGI 工厂"))?;
        let adapter = unsafe { factory.EnumAdapters1(mon.adapter_index) }.map_err(win("取显卡"))?;
        let output = unsafe { adapter.EnumOutputs(mon.output_index) }.map_err(win("取显示输出"))?;

        let (mut device, mut context): (Option<ID3D11Device>, Option<ID3D11DeviceContext>) = (None, None);
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(win("创建 D3D11 设备"))?;
        let (device, context) = (device.expect("设备已创建"), context.expect("上下文已创建"));

        // 优先 DuplicateOutput1 并同时要 FP16 与 BGRA8：HDR 开着时旧接口返回的 8 位图已被系统截断（高光丢失）；
        // 只有 Windows 10 1709 之前没有 IDXGIOutput5，那时才用旧接口
        let dup = match output.cast::<IDXGIOutput5>() {
            Ok(o5) => unsafe { o5.DuplicateOutput1(&device, 0, &[DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_B8G8R8A8_UNORM]) }.map_err(win("DuplicateOutput1"))?,
            Err(_) => {
                let output1: IDXGIOutput1 = output.cast().map_err(win("取 IDXGIOutput1"))?;
                unsafe { output1.DuplicateOutput(&device) }.map_err(win("DuplicateOutput"))?
            }
        };
        let dd: DXGI_OUTDUPL_DESC = unsafe { dup.GetDesc() };
        let (src_w, src_h) = (dd.ModeDesc.Width, dd.ModeDesc.Height);
        let tonemap = match dd.ModeDesc.Format {
            f if f == DXGI_FORMAT_B8G8R8A8_UNORM => None,
            f if f == DXGI_FORMAT_R16G16B16A16_FLOAT => {
                let nits = sdr_white_nits(&mon.device_name).unwrap_or(80.0);
                Some(Tonemap::new(&device, src_w, src_h, nits)?)
            }
            f => return Err(fail(ErrorKind::Unsupported, format!("桌面图像格式 {} 暂不支持", f.0))),
        };
        if dd.Rotation != DXGI_MODE_ROTATION_IDENTITY && dd.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED {
            return Err(fail(ErrorKind::Unsupported, format!("复制对象报告旋转 {}，暂不支持", dd.Rotation.0)));
        }
        if dd.DesktopImageInSystemMemory.as_bool() {
            return Err(fail(ErrorKind::Unsupported, "桌面图像在系统内存里（没有 GPU 复制路径）"));
        }

        let vdev: ID3D11VideoDevice = device.cast().map_err(win("取 ID3D11VideoDevice（显卡不支持视频处理？）"))?;
        let vctx: ID3D11VideoContext = context.cast().map_err(win("取 ID3D11VideoContext"))?;
        let vctx1: ID3D11VideoContext1 = vctx.cast().map_err(win("取 ID3D11VideoContext1"))?;

        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
            InputWidth: src_w,
            InputHeight: src_h,
            OutputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
            OutputWidth: out_w,
            OutputHeight: out_h,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let enumerator = unsafe { vdev.CreateVideoProcessorEnumerator(&content) }.map_err(win("创建视频处理器枚举器"))?;
        let can_in = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM) }.map_err(win("检查 BGRA 输入支持"))?;
        let can_out = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12) }.map_err(win("检查 NV12 输出支持"))?;
        if can_in & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 == 0 {
            return Err(fail(ErrorKind::Unsupported, "视频处理器不支持 BGRA 输入"));
        }
        if can_out & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
            return Err(fail(ErrorKind::Unsupported, "视频处理器不支持 NV12 输出"));
        }
        let processor = unsafe { vdev.CreateVideoProcessor(&enumerator, 0) }.map_err(win("创建视频处理器"))?;

        let mut nv12 = None;
        unsafe { device.CreateTexture2D(&tex_desc(out_w, out_h), None, Some(&mut nv12)) }.map_err(win("创建 NV12 输出纹理"))?;
        let nv12 = nv12.expect("纹理已创建");
        let mut staging = None;
        let mut sd = tex_desc(out_w, out_h);
        sd.Usage = D3D11_USAGE_STAGING;
        sd.BindFlags = 0;
        sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        unsafe { device.CreateTexture2D(&sd, None, Some(&mut staging)) }.map_err(win("创建 NV12 回读纹理（显卡不支持 NV12 的 CPU 读取？）"))?;
        let staging = staging.expect("纹理已创建");

        let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
        };
        let mut out_view = None;
        unsafe { vdev.CreateVideoProcessorOutputView(&nv12, &enumerator, &out_desc, Some(&mut out_view)) }.map_err(win("创建输出视图"))?;
        let out_view = out_view.expect("视图已创建");

        // 画面：整块源放进输出里，比例不同时留黑边；输出尺寸在整场录制里不变
        let layout = letterbox(src_w, src_h, out_w, out_h);
        unsafe {
            vctx1.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            vctx1.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            vctx1.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&rect(Rect { x: 0, y: 0, w: src_w, h: src_h })));
            vctx1.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&rect(layout)));
            vctx1.VideoProcessorSetOutputTargetRect(&processor, true, Some(&rect(Rect { x: 0, y: 0, w: out_w, h: out_h })));
            // 黑边必须是有限范围的黑（Y16、U/V128）：用 RGB 黑色设背景时驱动给出的是 Y0（实测），低于有限范围下限
            let black = D3D11_VIDEO_COLOR { Anonymous: D3D11_VIDEO_COLOR_0 { YCbCr: D3D11_VIDEO_COLOR_YCbCrA { Y: 16.0 / 255.0, Cb: 128.0 / 255.0, Cr: 128.0 / 255.0, A: 1.0 } } };
            vctx1.VideoProcessorSetOutputBackgroundColor(&processor, true, &black);
            // 输入是 sRGB 全范围，输出是 BT.709 有限范围（编码器与播放器按它解读）
            vctx1.VideoProcessorSetStreamColorSpace1(&processor, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
            vctx1.VideoProcessorSetOutputColorSpace1(&processor, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
        }

        Ok(Self { device, context, dup, tonemap, vctx: vctx1, vdev, enumerator, processor, nv12, out_view, staging, src_w, src_h, out_w, out_h, layout, src_format: None })
    }

    /// 桌面是 HDR（FP16）：画面要先经过 HDR→SDR 转换。
    pub fn is_hdr(&self) -> bool {
        self.tonemap.is_some()
    }

    /// 用户调了系统的“SDR 内容亮度”：更新转换增益。非 HDR 管线上无作用。
    pub fn set_sdr_white_nits(&self, nits: f32) {
        if let Some(t) = &self.tonemap {
            t.set_white_nits(&self.context, nits);
        }
    }

    /// 取一帧：桌面有更新就转成 NV12 返回，没有更新返回 `None`（调用方沿用上一帧）。
    /// 以 0 超时调用 `AcquireNextFrame`，不会阻塞等待屏幕变化。
    pub fn grab(&mut self) -> Result<Option<Arc<[u8]>>, PipelineError> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match unsafe { self.dup.AcquireNextFrame(0, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(e) => return Err(win("AcquireNextFrame")(e)),
        }
        // LastPresentTime 为 0 表示只有鼠标指针动了，桌面图像没有更新
        let result = if info.LastPresentTime == 0 { Ok(None) } else { self.convert(resource).map(Some) };
        // 无论转换成功与否都必须释放，否则下一次 Acquire 会报调用无效
        let _ = unsafe { self.dup.ReleaseFrame() };
        result
    }

    fn convert(&mut self, resource: Option<IDXGIResource>) -> Result<Arc<[u8]>, PipelineError> {
        let texture: ID3D11Texture2D = resource.ok_or_else(|| fail(ErrorKind::Other, "AcquireNextFrame 没有给出资源"))?.cast().map_err(win("桌面资源转纹理"))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        self.src_format = Some(desc.Format.0);
        if (desc.Width, desc.Height) != (self.src_w, self.src_h) {
            // 显示模式已经变了，复制对象马上就会报失效；按失效处理，让上层重建
            return Err(fail(ErrorKind::AccessLost, format!("桌面尺寸从 {}×{} 变成 {}×{}", self.src_w, self.src_h, desc.Width, desc.Height)));
        }

        let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
        };
        // HDR 时先转成 SDR 的 BGRA8，视频处理器的输入永远是 sRGB 的 BGRA8
        let input: &ID3D11Texture2D = match &self.tonemap {
            Some(t) => t.apply(&self.device, &self.context, &texture)?,
            None => &texture,
        };
        let mut in_view = None;
        unsafe { self.vdev.CreateVideoProcessorInputView(input, &self.enumerator, &in_desc, Some(&mut in_view)) }.map_err(win("创建输入视图"))?;
        let stream = D3D11_VIDEO_PROCESSOR_STREAM { Enable: true.into(), pInputSurface: std::mem::ManuallyDrop::new(in_view), ..Default::default() };
        let blt = unsafe { self.vctx.VideoProcessorBlt(&self.processor, &self.out_view, 0, std::slice::from_ref(&stream)) };
        // 视图的引用要自己放：结构体里是 ManuallyDrop
        drop(std::mem::ManuallyDrop::into_inner(stream.pInputSurface));
        blt.map_err(win("VideoProcessorBlt"))?;

        unsafe { self.context.CopyResource(&self.staging, &self.nv12) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe { self.context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.map_err(win("Map 回读纹理"))?;
        let frame = self.copy_planes(&mapped);
        unsafe { self.context.Unmap(&self.staging, 0) };
        Ok(frame)
    }

    /// 把带行距的 NV12 紧凑地复制出来：Y 平面 `h` 行，紧接着 UV 平面 `h/2` 行，行距相同。
    fn copy_planes(&self, mapped: &D3D11_MAPPED_SUBRESOURCE) -> Arc<[u8]> {
        let (w, h) = (self.out_w as usize, self.out_h as usize);
        let pitch = mapped.RowPitch as usize;
        let base = mapped.pData as *const u8;
        let mut frame: Arc<[u8]> = unsafe { Arc::<[u8]>::new_zeroed_slice(nv12_frame_bytes(self.out_w, self.out_h)).assume_init() };
        let dst = Arc::get_mut(&mut frame).expect("刚创建，没有其它引用");
        for row in 0..h + h / 2 {
            // 行 0..h 是 Y，之后是 UV；两个平面行距相同，UV 平面从第 h 行开始
            let src = unsafe { std::slice::from_raw_parts(base.add(row * pitch), w) };
            dst[row * w..(row + 1) * w].copy_from_slice(src);
        }
        frame
    }
}
