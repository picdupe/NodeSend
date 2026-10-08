//! 文件功能：Cargo 构建脚本。
//!
//! 职责：
//! 1. 调用 Tauri 构建管线（生成 IPC 校验代码、读取 tauri.conf.json）；
//! 2. Windows 下为【测试目标】嵌入 Common-Controls v6 清单：
//!    tao 使用 comctl32 的 TaskDialogIndirect，仅 v6 清单下可解析，
//!    普通 cargo test exe 默认无此清单，会以 STATUS_ENTRYPOINT_NOT_FOUND 启动失败。
//!    使用 embed-resource 的 compile_for_tests + manifest_required，
//!    由其负责覆盖 rustc 默认嵌入的清单，只影响测试目标，不污染主程序。
fn main() {
    println!("cargo:rerun-if-env-changed=STATIC_VCRUNTIME");
    let mut attributes = tauri_build::Attributes::new();
    // Older CLI versions still inject this variable. Translate it into the
    // supported API while preserving the CLI's selected linking behavior.
    if let Some(value) = std::env::var_os("STATIC_VCRUNTIME") {
        std::env::remove_var("STATIC_VCRUNTIME");
        attributes = attributes.windows_attributes(
            tauri_build::WindowsAttributes::new().static_vc_runtime(value != "false"),
        );
    }
    tauri_build::try_build(attributes).expect("Tauri build failed");

    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_test_manifest();
    }
}

#[cfg(windows)]
fn embed_test_manifest() {
    use std::path::PathBuf;

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let manifest_path = out_dir.join("test.manifest");
    let rc_path = out_dir.join("test.rc");

    std::fs::write(&manifest_path, TEST_MANIFEST_XML).expect("写测试清单");
    // 资源脚本：资源 ID=1，类型 RT_MANIFEST(24)，文件用正斜杠绝对路径（兼容 rc.exe 与 windres）。
    std::fs::write(
        &rc_path,
        format!(
            "1 24 \"{}\"",
            manifest_path.display().to_string().replace('\\', "/")
        ),
    )
    .expect("写资源脚本");

    // 清单是测试运行的硬性前置（入口点解析），故 manifest_required：失败即中断构建。
    embed_resource::compile_for_tests(&rc_path, embed_resource::NONE)
        .manifest_required()
        .expect("为测试目标嵌入 Common-Controls v6 清单");
}

#[cfg(windows)]
const TEST_MANIFEST_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#;
