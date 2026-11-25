use crate::{error, modrinth, runtime, Result};
use base64::Engine;
use serde::Serialize;
#[derive(Serialize)]
pub struct Category {
    pub name: String,
}
#[tauri::command]
pub async fn catalog_categories() -> Result<Vec<Category>> {
    let value = runtime::get_json("https://api.modrinth.com/v2/tag/category").await?;
    let mut names = value
        .as_array()
        .ok_or("Missing categories")?
        .iter()
        .filter(|v| matches!(v["project_type"].as_str(), Some("mod" | "plugin")))
        .filter_map(|v| v["name"].as_str())
        .filter(|v| modrinth::identifier(v).is_ok())
        .map(str::to_string)
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    Ok(names.into_iter().map(|name| Category { name }).collect())
}
#[derive(Serialize)]
pub struct Gallery {
    pub title: String,
    pub image: String,
}
#[derive(Serialize)]
pub struct Details {
    pub title: String,
    pub body: String,
    pub categories: Vec<String>,
    pub gallery: Vec<Gallery>,
}
#[tauri::command]
pub async fn catalog_details(project_id: String) -> Result<Details> {
    modrinth::identifier(&project_id)?;
    let value =
        runtime::get_json(&format!("https://api.modrinth.com/v2/project/{project_id}")).await?;
    let mut gallery = Vec::new();
    for item in value["gallery"].as_array().into_iter().flatten().take(6) {
        let Some(url) = item["url"].as_str() else {
            continue;
        };
        if let Ok(image) = image(url).await {
            gallery.push(Gallery {
                title: item["title"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect(),
                image,
            });
        }
    }
    Ok(Details {
        title: value["title"].as_str().unwrap_or("").into(),
        body: value["body"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(100000)
            .collect(),
        categories: value["categories"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        gallery,
    })
}
async fn image(url: &str) -> Result<String> {
    provider_image(url, "cdn.modrinth.com").await
}
pub(crate) async fn provider_image(url: &str, host: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(url).map_err(error)?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some(host)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port_or_known_default() != Some(443)
    {
        return Err("Unapproved image host".into());
    }
    let image_client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("MineHost/0.8.0")
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(error)?;
    let mut response = image_client
        .get(url)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(error)?
        .error_for_status()
        .map_err(error)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(error)? {
        if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
            return Err("Image exceeds 2 MiB".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        "image/webp"
    } else {
        return Err("Only PNG, JPEG and WebP gallery images are allowed".into());
    };
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "Official Modrinth catalogue and gallery, GitHub Actions only"]
    async fn catalog_contract() {
        assert!(!catalog_categories().await.unwrap().is_empty());
        let details = catalog_details("chunky".into()).await.unwrap();
        assert!(!details.title.is_empty() && !details.body.is_empty());
        for item in details.gallery {
            assert!(item.image.starts_with("data:image/"));
        }
        assert!(image("https://example.com/image.svg").await.is_err());
    }
}
