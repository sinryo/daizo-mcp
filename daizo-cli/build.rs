use std::process::Command;

fn main() {
    // ビルド日時を設定
    let output = Command::new("date")
        .args(["+%Y-%m-%d %H:%M:%S"])
        .output()
        .expect("Failed to execute date command");
    let build_date = String::from_utf8_lossy(&output.stdout).trim().to_string();
    println!("cargo:rustc-env=BUILD_DATE={}", build_date);

    // Git コミットハッシュを取得（利用可能な場合）
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
    {
        if output.status.success() {
            let git_hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("cargo:rustc-env=GIT_HASH={}", git_hash);
        }
    }

    // ビルドのたびに再実行
    println!("cargo:rerun-if-changed=build.rs");
}

