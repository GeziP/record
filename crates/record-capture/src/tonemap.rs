//! HDR → SDR（设计文档 5.2 的 HDR 策略 ②）。
//!
//! 显示器开着 HDR 时，桌面在系统里是 scRGB（FP16 线性，1.0 = 80 尼特），SDR 内容被放大到“SDR 内容亮度”
//! （本机 240 尼特即放大 3 倍）。旧版 `DuplicateOutput` 直接把它截断成 8 位，高光信息当场丢失，事后无法修复；
//! 所以要用 `DuplicateOutput1` 要 FP16，再在 GPU 上除掉这个放大比例，截断到 SDR 范围，做 sRGB 编码。
//! HDR 高光会被截断（输出本来就是 SDR 的 H.264）。

use crate::pipeline::{ErrorKind, PipelineError, fail, win};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BUFFER_DESC, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT,
    ID3D11Buffer, ID3D11Device, ID3D11DeviceContext, ID3D11PixelShader, ID3D11RenderTargetView, ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::core::s;

const HLSL: &str = r#"
cbuffer Params : register(b0) { float gain; float3 pad; };
Texture2D<float4> src : register(t0);
struct VSOut { float4 pos : SV_Position; };
VSOut vs_main(uint id : SV_VertexID) {
    float2 uv = float2((id << 1) & 2, id & 2);
    VSOut o;
    o.pos = float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}
float3 encode(float3 c) {
    float3 lo = c * 12.92;
    float3 hi = 1.055 * pow(c, 1.0 / 2.4) - 0.055;
    return lerp(hi, lo, step(c, 0.0031308));
}
float4 ps_main(VSOut i) : SV_Target {
    float3 lin = saturate(src.Load(int3(i.pos.xy, 0)).rgb * gain);
    return float4(encode(lin), 1);
}
"#;

/// 系统“SDR 内容亮度”换算成线性增益：scRGB 的 1.0 是 80 尼特。
pub fn gain_for(white_nits: f32) -> f32 {
    80.0 / white_nits.max(1.0)
}

fn compile(entry: windows::core::PCSTR, target: windows::core::PCSTR) -> Result<ID3DBlob, PipelineError> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let r = unsafe {
        D3DCompile(
            HLSL.as_ptr().cast(),
            HLSL.len(),
            s!("tonemap.hlsl"),
            None,
            None::<&windows::Win32::Graphics::Direct3D::ID3DInclude>,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = r {
        let detail = errors.map(|b| blob_text(&b)).unwrap_or_default();
        return Err(fail(ErrorKind::Other, format!("编译 HDR 转换着色器失败：{e} {detail}")));
    }
    code.ok_or_else(|| fail(ErrorKind::Other, "着色器编译没有产出"))
}

fn blob_bytes(b: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }
}

fn blob_text(b: &ID3DBlob) -> String {
    String::from_utf8_lossy(blob_bytes(b)).trim().to_string()
}

pub struct Tonemap {
    sdr: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    w: u32,
    h: u32,
}

impl Tonemap {
    pub fn new(device: &ID3D11Device, w: u32, h: u32, white_nits: f32) -> Result<Self, PipelineError> {
        let vs_code = compile(s!("vs_main"), s!("vs_5_0"))?;
        let ps_code = compile(s!("ps_main"), s!("ps_5_0"))?;
        let mut vs = None;
        unsafe { device.CreateVertexShader(blob_bytes(&vs_code), None, Some(&mut vs)) }.map_err(win("创建顶点着色器"))?;
        let mut ps = None;
        unsafe { device.CreatePixelShader(blob_bytes(&ps_code), None, Some(&mut ps)) }.map_err(win("创建像素着色器"))?;

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
        let mut sdr = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut sdr)) }.map_err(win("创建 SDR 中间纹理"))?;
        let sdr: ID3D11Texture2D = sdr.expect("纹理已创建");
        let mut rtv = None;
        unsafe { device.CreateRenderTargetView(&sdr, None, Some(&mut rtv)) }.map_err(win("创建渲染目标视图"))?;

        let gain = [gain_for(white_nits), 0.0, 0.0, 0.0];
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: gain.as_ptr().cast(), SysMemPitch: 0, SysMemSlicePitch: 0 };
        let bd = D3D11_BUFFER_DESC { ByteWidth: 16, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, CPUAccessFlags: 0, MiscFlags: 0, StructureByteStride: 0 };
        let mut cb = None;
        unsafe { device.CreateBuffer(&bd, Some(&init), Some(&mut cb)) }.map_err(win("创建常量缓冲"))?;

        Ok(Self { sdr, rtv: rtv.expect("视图已创建"), vs: vs.expect("着色器已创建"), ps: ps.expect("着色器已创建"), cb: cb.expect("缓冲已创建"), w, h })
    }

    /// 用户调了“SDR 内容亮度”滑块时更新增益。
    pub fn set_white_nits(&self, context: &ID3D11DeviceContext, white_nits: f32) {
        let gain = [gain_for(white_nits), 0.0, 0.0, 0.0];
        unsafe { context.UpdateSubresource(&self.cb, 0, None, gain.as_ptr().cast(), 0, 0) };
    }

    /// 把 FP16 的桌面图像转成 SDR 的 BGRA8，返回结果纹理。
    pub fn apply(&self, device: &ID3D11Device, context: &ID3D11DeviceContext, src: &ID3D11Texture2D) -> Result<&ID3D11Texture2D, PipelineError> {
        let mut srv = None;
        unsafe { device.CreateShaderResourceView(src, None, Some(&mut srv)) }.map_err(win("创建桌面纹理的着色器视图"))?;
        unsafe {
            context.OMSetRenderTargets(Some(&[Some(self.rtv.clone())]), None);
            context.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: self.w as f32, Height: self.h as f32, MinDepth: 0.0, MaxDepth: 1.0 }]));
            context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(&self.vs, None);
            context.PSSetShader(&self.ps, None);
            context.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            context.PSSetShaderResources(0, Some(&[srv]));
            context.Draw(3, 0);
            // 解绑：后面这张纹理要当视频处理器的输入，不能同时还挂在渲染目标上
            context.PSSetShaderResources(0, Some(&[None]));
            context.OMSetRenderTargets(None, None);
        }
        Ok(&self.sdr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gain_divides_out_the_sdr_white_level() {
        assert_eq!(gain_for(80.0), 1.0);
        assert!((gain_for(240.0) - 1.0 / 3.0).abs() < 1e-6);
        assert!((gain_for(160.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_nonsense_white_level_does_not_blow_up() {
        assert!(gain_for(0.0).is_finite() && gain_for(0.0) > 0.0);
        assert!(gain_for(-5.0).is_finite());
    }
}
