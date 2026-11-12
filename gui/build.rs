fn main() {
    // Fluent 스타일: Windows 11 기본 컨트롤과 비슷한 모양(밝은/어두운 테마 자동 전환).
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint UI 컴파일 실패");
    // 실행 파일 아이콘과 버전 정보
    embed_resource::compile("app.rc", embed_resource::NONE).manifest_optional().unwrap();
}
