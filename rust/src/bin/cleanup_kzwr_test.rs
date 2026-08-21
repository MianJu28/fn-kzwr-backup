//! 清理 kzwr 测试残留：删除整个 fn-backup 文件夹
//!
//! 流程：逻辑删除 /fn-backup 文件夹(进回收站) → 从回收站物理删除(purge)
//! 需要 TRIM_KZWR_TOKEN 环境变量。

use anyhow::Result;

use fnos_backup::infra::target::kzwr::client::KzwrClient;

const TARGET_FOLDER_NAME: &str = "fn-backup";

#[tokio::main]
async fn main() -> Result<()> {
    let token = std::env::var("TRIM_KZWR_TOKEN")
        .unwrap_or_else(|_| panic!("请设置 TRIM_KZWR_TOKEN 环境变量为 access-token"));
    let base_url = std::env::var("TRIM_KZWR_BASE_URL")
        .unwrap_or_else(|_| "https://www.kzwr.com".to_string());

    let mut client = KzwrClient::new(&base_url, 30);
    client.set_token(&token);

    // 1) 找 /fn-backup 文件夹的 encodedId
    println!("=== 1. 定位 /fn-backup 文件夹 ===");
    let files = client.list_files("/", 1, true).await?;
    let encoded_id = files
        .get("folders")
        .and_then(|f| f.as_array())
        .and_then(|arr| {
            arr.iter().find(|it| {
                it.get("name")
                    .or_else(|| it.get("folderName"))
                    .and_then(|v| v.as_str())
                    == Some(TARGET_FOLDER_NAME)
            })
        })
        .and_then(|it| it.get("encodedId").and_then(|v| v.as_str()))
        .map(|s| s.to_string());

    let encoded_id = match encoded_id {
        Some(id) => {
            println!("[+] 找到 fn-backup encodedId: {}", id);
            id
        }
        None => {
            println!("[*] 根目录下未找到 fn-backup 文件夹");
            return Ok(());
        }
    };

    // 2) 逻辑删除整个 fn-backup 文件夹（进回收站）
    println!("=== 2. 逻辑删除 /fn-backup（进回收站）===");
    client
        .delete_folder(&[encoded_id.clone()], false)
        .await
        .map_err(|e| anyhow::anyhow!("逻辑删除失败: {}", e))?;
    println!("[+] 逻辑删除成功");

    // 3) 从回收站物理删除 fn-backup 文件夹
    println!("=== 3. 从回收站物理删除(purge) ===");
    let trash = client.get_trash(1).await?;
    let items = trash
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // 找到回收站里的 fn-backup 文件夹条目
    let folder_items: Vec<_> = items
        .iter()
        .filter(|it| {
            it.get("itemType").and_then(|v| v.as_str()) == Some("folder")
                && it.get("name").and_then(|v| v.as_str()) == Some(TARGET_FOLDER_NAME)
        })
        .cloned()
        .collect();

    if folder_items.is_empty() {
        println!("[*] 回收站中未找到 fn-backup 文件夹");
    } else {
        let ids: Vec<String> = folder_items
            .iter()
            .filter_map(|it| it.get("encodedId").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .collect();
        client.delete_trash_folders(&ids).await?;
        println!("[+] 已物理删除 {} 个 fn-backup 文件夹", ids.len());
    }

    // 4) 验证根目录已无 fn-backup
    println!("=== 4. 验证 ===");
    let files = client.list_files("/", 1, true).await?;
    let remaining = files
        .get("folders")
        .and_then(|f| f.as_array())
        .map(|arr| arr.iter().filter(|it| it.get("name").and_then(|v| v.as_str()) == Some(TARGET_FOLDER_NAME)).count())
        .unwrap_or(0);
    if remaining == 0 {
        println!("[+] /fn-backup 已完全清理");
    } else {
        println!("[!] 仍有 {} 个 fn-backup 文件夹", remaining);
    }

    println!("\n=== 清理完成 ===");
    Ok(())
}
