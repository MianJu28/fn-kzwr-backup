//! kzwr API 重写验证测试
//!
//! 读取 TRIM_KZWR_TOKEN 环境变量，调用重写的 Rust API 客户端，
//! 验证 get_member / list_files 是否正确工作。

use fnos_backup::infra::target::kzwr::client::KzwrClient;
use fnos_backup::infra::target::kzwr::storage::KzwrTarget;
use fnos_backup::infra::storage_trait::TargetStorage;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let token = std::env::var("TRIM_KZWR_TOKEN")
        .unwrap_or_else(|_| panic!("请设置 TRIM_KZWR_TOKEN 环境变量为 access-token"));

    let base_url = std::env::var("TRIM_KZWR_BASE_URL")
        .unwrap_or_else(|_| "https://www.kzwr.com".to_string());

    let mut client = KzwrClient::new(&base_url, 30);
    client.set_token(&token);
    println!("[*] 已设置 token (长度 {})", token.len());

    // 1) get_member
    println!("\n=== 1. get_member ===");
    match client.get_member().await {
        Ok(v) => println!("[+] 用户信息: {}", serde_json::to_string_pretty(&v).unwrap()),
        Err(e) => println!("[!] get_member 失败: {:?}", e),
    }

    // 2) list_files 根目录
    println!("\n=== 2. list_files(/) ===");
    match client.list_files("/", 1, true).await {
        Ok(v) => {
            let files = v
                .get("data")
                .and_then(|d| d.get("files"))
                .and_then(|f| f.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            let folders = v
                .get("data")
                .and_then(|d| d.get("folders"))
                .and_then(|f| f.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            println!("[+] 根目录: {} 个文件, {} 个文件夹", files, folders);
            println!("[+] 响应摘要: {}", serde_json::to_string_pretty(&v).unwrap());
        }
        Err(e) => println!("[!] list_files 失败: {:?}", e),
    }

    // 3) 通过 TargetStorage::list 列出根目录（验证 trait 适配器字段定位修复）
    println!("\n=== 3. KzwrTarget::list(/) ===");
    let target = KzwrTarget::new(client);
    match target.list("/").await {
        Ok(entries) => {
            println!("[+] 根目录共 {} 个条目:", entries.len());
            for e in &entries {
                println!(
                    "    - [{}] {} (size={})",
                    if e.is_dir { "DIR" } else { "FILE" },
                    e.rel_path,
                    e.size
                );
            }
        }
        Err(e) => println!("[!] KzwrTarget::list 失败: {:?}", e),
    }

    // 4) ping（验证认证）
    println!("\n=== 4. KzwrTarget::ping ===");
    match target.ping().await {
        Ok(_) => println!("[+] ping 通过（凭证有效）"),
        Err(e) => println!("[!] ping 失败: {:?}", e),
    }

    Ok(())
}
