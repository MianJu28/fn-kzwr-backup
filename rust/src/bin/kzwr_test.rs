//! kzwr API 重写验证测试
//!
//! 读取 TRIM_KZWR_TOKEN 环境变量，调用重写的 Rust API 客户端，
//! 验证：get_member / list_files / 分片上传 / 下载一致性。

use std::path::PathBuf;

use fnos_backup::infra::target::kzwr::client::KzwrClient;
use fnos_backup::infra::target::kzwr::storage::KzwrTarget;
use fnos_backup::infra::target::kzwr::upload::upload_file;
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

    // 2) 分片上传
    println!("\n=== 2. 分片上传测试 ===");
    let upload_test = upload_test(&client).await;

    // 3) 上传后列表确认
    println!("\n=== 3. 上传后 KzwrTarget::list(share) ===");
    let target = KzwrTarget::new(client);
    match target.list("/share").await {
        Ok(entries) => {
            println!("[+] share 目录共 {} 个条目:", entries.len());
            for e in &entries {
                println!(
                    "    - [{}] {} (size={})",
                    if e.is_dir { "DIR" } else { "FILE" },
                    e.rel_path,
                    e.size
                );
            }
        }
        Err(e) => println!("[!] list 失败: {:?}", e),
    }

    // 4) 文件夹创建/删除测试
    println!("\n=== 4. 文件夹创建/删除测试 ===");
    folder_test(target.client()).await;

    // 5) 下载并对比（若上传成功）
    if let Some(uploaded) = &upload_test {
        println!("\n=== 5. 下载一致性测试 ===");
        download_test(&target, uploaded).await;
    }

    // 6) 删除测试：删除 share 下所有 rust_upload_test_*.bin 测试文件
    println!("\n=== 6. 删除测试（清理测试文件）===");
    delete_test_files(&target).await;

    // 7) ping
    println!("\n=== 7. KzwrTarget::ping ===");
    match target.ping().await {
        Ok(_) => println!("[+] ping 通过（凭证有效）"),
        Err(e) => println!("[!] ping 失败: {:?}", e),
    }

    // 清理本地生成的测试文件
    if let Some(uploaded) = &upload_test {
        let _ = std::fs::remove_file(&uploaded.local_path);
        println!("[*] 已清理本地测试文件: {}", uploaded.local_path.display());
    }

    Ok(())
}

/// 删除 share 目录下所有 rust_upload_test_*.bin 测试文件
async fn delete_test_files(target: &KzwrTarget) {
    let entries = match target.list("/share").await {
        Ok(e) => e,
        Err(e) => {
            println!("[!] 列出 share 失败: {:?}", e);
            return;
        }
    };

    let mut deleted = 0;
    for e in entries {
        if e.rel_path.contains("rust_upload_test_") {
            let rel = std::path::PathBuf::from(e.rel_path.clone());
            match target.delete(&rel).await {
                Ok(_) => {
                    println!("[+] 已删除: {}", e.rel_path);
                    deleted += 1;
                }
                Err(err) => println!("[!] 删除 {} 失败: {:?}", e.rel_path, err),
            }
        }
    }

    if deleted > 0 {
        // 再次列出确认
        match target.list("/share").await {
            Ok(entries2) => {
                let remaining: Vec<_> = entries2
                    .iter()
                    .filter(|x| x.rel_path.contains("rust_upload_test_"))
                    .collect();
                if remaining.is_empty() {
                    println!("[+] 所有测试文件已删除，share 目录清理干净");
                } else {
                    println!("[!] 仍有 {} 个测试文件未删除", remaining.len());
                }
            }
            Err(e) => println!("[!] 删除后列表失败: {:?}", e),
        }
    } else {
        println!("[*] 未发现需要删除的测试文件");
    }
}

/// 文件夹创建/删除测试（对照 Python create_folder/delete_folder 逻辑）
async fn folder_test(client: &KzwrClient) {
    // 1) 创建测试文件夹
    let folder_name = format!(
        "rust_folder_test_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    );
    println!("[*] 创建文件夹: {}", folder_name);
    let created = match client.create_folder(&folder_name, "/").await {
        Ok(p) => p,
        Err(e) => {
            println!("[!] create_folder 失败: {:?}", e);
            return;
        }
    };
    println!("[+] create_folder 返回路径: {}", created);

    // 2) 列出根文件夹，找到新建文件夹的 encodedId
    // 注：list_folders 返回的 id 是短码，删除需用 list_files 的 folders[].encodedId
    println!("[*] 查找文件夹 encodedId ...");
    let mut encoded_id = None;
    if let Ok(files) = client.list_files("/", 1, true).await {
        encoded_id = find_folder_encoded_id(&files, &folder_name);
    }
    let encoded_id = match encoded_id {
        Some(id) => {
            println!("[+] 找到文件夹 encodedId: {}", id);
            id
        }
        None => {
            println!("[!] 在根目录未找到新文件夹（删除测试跳过）");
            return;
        }
    };

    // 3) 逻辑删除文件夹（对照 Python delete_folder physical=False）
    println!("[*] 逻辑删除文件夹 ...");
    match client
        .delete_folder(&[encoded_id.to_string()], false)
        .await
    {
        Ok(_) => println!("[+] 文件夹删除成功"),
        Err(e) => println!("[!] delete_folder 失败: {:?}", e),
    }
}

/// 从 list_folders 响应中按名字找文件夹 encodedId
fn find_folder_encoded_id(resp: &serde_json::Value, name: &str) -> Option<String> {
    // 优先顶层 folders，兼容 data.folders
    let folders = resp
        .get("folders")
        .or_else(|| resp.get("data").and_then(|d| d.get("folders")))
        .and_then(|f| f.as_array())
        .cloned()
        .unwrap_or_default();
    folders
        .iter()
        .find(|it| {
            it.get("name")
                .or_else(|| it.get("folderName"))
                .and_then(|v| v.as_str())
                == Some(name)
        })
        .and_then(|it| {
            it.get("encodedId")
                .or_else(|| it.get("id"))
                .and_then(|v| v.as_str())
        })
        .map(|s| s.to_string())
}

/// 上传信息
struct Uploaded {
    folder: String,
    file_name: String,
    local_path: PathBuf,
}

/// 分片上传测试：生成 ~10MB 随机文件，上传到 share 目录
async fn upload_test(client: &KzwrClient) -> Option<Uploaded> {
    // 生成测试文件（含随机数据，超过 4MB 触发多分片）
    let dir = std::env::temp_dir();
    let file_name = format!("rust_upload_test_{}.bin", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
    let local_path = dir.join(&file_name);

    println!("[*] 生成测试文件: {}", local_path.display());
    if let Err(e) = generate_test_file(&local_path, 10 * 1024 * 1024 + 1234) {
        println!("[!] 生成测试文件失败: {}", e);
        return None;
    }

    // 上传到 /share 目录
    println!("[*] 分片上传到 /share ...");
    match upload_file(client, &local_path, "/share", "Auto", 0, 3).await {
        Ok(resp) => {
            println!("[+] 上传完成! 响应: {}", serde_json::to_string_pretty(&resp).unwrap());
            Some(Uploaded {
                folder: "/share".to_string(),
                file_name,
                local_path,
            })
        }
        Err(e) => {
            println!("[!] 上传失败: {:?}", e);
            None
        }
    }
}

/// 下载并对比内容一致性
async fn download_test(target: &KzwrTarget, uploaded: &Uploaded) {
    // 下载到本地临时文件
    let download_path = std::env::temp_dir().join(format!("{}.download", uploaded.file_name));
    let rel_path = PathBuf::from(format!("{}/{}", uploaded.folder.trim_start_matches('/'), uploaded.file_name));

    // 用 TargetStorage::read_stream 下载
    println!("[*] 下载 {} 到 {}", rel_path.display(), download_path.display());
    let bytes = match target.read_stream(&rel_path).await {
        Ok(stream) => {
            use futures::StreamExt;
            let mut buf = Vec::new();
            let mut stream = Box::pin(stream);
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(b) => buf.extend_from_slice(&b),
                    Err(e) => {
                        println!("[!] 下载流错误: {:?}", e);
                        return;
                    }
                }
            }
            buf
        }
        Err(e) => {
            println!("[!] 下载失败: {:?}", e);
            return;
        }
    };

    // 对比内容
    let original = match std::fs::read(&uploaded.local_path) {
        Ok(b) => b,
        Err(e) => {
            println!("[!] 读取原文件失败: {}", e);
            return;
        }
    };

    if bytes.len() == original.len() && bytes == original {
        println!("[+] 一致性校验通过! 大小 = {} 字节", bytes.len());
        println!("[+] 原文件 SHA256 = {}", sha256_hex(&original));
        println!("[+] 下载文件 SHA256 = {}", sha256_hex(&bytes));
    } else {
        println!(
            "[!] 不一致! 原文件 {} 字节 vs 下载 {} 字节",
            original.len(),
            bytes.len()
        );
    }

    // 清理下载的临时文件
    let _ = std::fs::remove_file(&download_path);
}

/// 生成指定大小的测试文件（伪随机，避免全是 0）
fn generate_test_file(path: &PathBuf, size: usize) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    // 用简单 PRNG 生成可复现的伪随机数据
    let mut seed: u64 = 0x1234567890ABCDEF;
    let mut buf = [0u8; 8192];
    let mut remaining = size;
    while remaining > 0 {
        let n = remaining.min(buf.len());
        for b in buf.iter_mut() {
            // xorshift64
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *b = (seed & 0xFF) as u8;
        }
        f.write_all(&buf[..n])?;
        remaining -= n;
    }
    f.flush()
}

/// SHA256 十六进制（用于校验对比）
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}
