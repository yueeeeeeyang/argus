//! 文件职责：使用内置 HTTP 客户端实现 Subversion WebDAV/DeltaV 仓库的只读访问。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：发现 SVN HTTPv2 修订资源、枚举目录、读取文件，并安全处理认证与重定向。

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::DateTime;
use percent_encoding::percent_decode_str;
use roxmltree::{Document, Node};
use url::Url;

use crate::remote::connection::SvnLinkConfig;
use crate::remote::remote_file::{RemoteFileEntry, RemoteFileEntryKind, remote_child_path};

/// HTTP 连接超时，避免不可达内网地址长期占用单个仓库 worker。
const SVN_HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 单次 DAV 请求读写超时；大文件下载按持续有数据到达计算，不限制总耗时。
const SVN_HTTP_IO_TIMEOUT: Duration = Duration::from_secs(60);
/// 单次 DAV XML 响应最大体积，防止异常服务返回无界元数据占用内存。
const SVN_HTTP_MAX_XML_BYTES: u64 = 16 * 1024 * 1024;
/// 同源重定向最大次数，兼容仓库根目录补斜杠等常见服务器配置。
const SVN_HTTP_MAX_REDIRECTS: usize = 5;
/// SVN HTTPv2 能力发现请求体；服务端据此返回最新修订号和修订资源根等专用响应头。
const SVN_DAV_OPTIONS_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:options xmlns:D="DAV:">
  <D:activity-collection-set/>
</D:options>"#;
/// DAV 属性查询请求体；只请求文件管理页实际使用的类型、名称、大小和时间字段。
const SVN_DAV_PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:resourcetype/>
    <D:displayname/>
    <D:getcontentlength/>
    <D:getlastmodified/>
    <D:creationdate/>
  </D:prop>
</D:propfind>"#;

/// HTTP SVN 文件属性，供上层在下载和预览前再次确认节点类型与大小。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct HttpSvnNodeStat {
    /// DAV 资源映射后的通用节点类型。
    pub kind: RemoteFileEntryKind,
    /// 普通文件字节数；服务端没有返回时为空。
    pub size: Option<u64>,
}

/// OPTIONS 返回的 SVN HTTPv2 仓库发现信息。
#[derive(Clone, Debug)]
struct SvnDavDiscovery {
    /// OPTIONS 最终落到的同源 URL，用于处理仓库根目录补斜杠重定向。
    request_url: Url,
    /// 仓库当前最新修订号。
    youngest_revision: u64,
    /// 仓库公共根 URL，用于计算链接 URL 在仓库内的浏览根路径。
    repository_root: Url,
    /// HTTPv2 修订根桩 URL，后续追加 `修订号/仓库路径` 读取历史快照。
    revision_root_stub: Url,
}

/// 从 DAV multistatus XML 中解析出的单个资源。
#[derive(Clone, Debug, Eq, PartialEq)]
struct DavResource {
    /// 服务端返回的规范资源 URL。
    url: Url,
    /// DAV displayname 或 URL 最后一段解码后的名称。
    name: String,
    /// collection 映射为目录，其余映射为普通文件。
    kind: RemoteFileEntryKind,
    /// 普通文件大小。
    size: Option<u64>,
    /// 最后修改时间或创建时间的 Unix 秒。
    mtime: Option<u64>,
}

/// 内置 SVN HTTPv2 只读会话；所有请求复用连接池，但不创建 working copy 或本地缓存。
pub(super) struct HttpSvnSession {
    /// 禁止自动重定向的 HTTP agent；重定向由本类型执行同源和降级检查。
    agent: ureq::Agent,
    /// 已移除 URL 用户信息的浏览根 URL。
    base_url: Url,
    /// 可选 HTTP Basic Authorization 请求头；只保存在内存中。
    authorization: Option<String>,
    /// SVN HTTPv2 修订根桩 URL。
    revision_root_stub: Url,
    /// 链接 URL 相对真实仓库根的路径段，保证浏览不能越过用户配置的位置。
    browse_root_segments: Vec<String>,
    /// 首次 OPTIONS 已取得但尚未被上层消费的最新修订号。
    initial_youngest_revision: Option<u64>,
}

impl HttpSvnSession {
    /// 从链接配置创建 HTTP SVN 会话并完成 HTTPv2 能力发现。
    ///
    /// 参数：`config` 提供仓库 URL 和可选用户名/密码；密码不会写入 URL 或错误文本。
    /// 返回值：服务端提供 HTTPv2 修订根信息时返回可用会话，否则返回明确的不兼容错误。
    pub(super) fn open(config: &SvnLinkConfig) -> Result<Self> {
        let (base_url, authorization) = sanitized_url_and_authorization(config)?;

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(SVN_HTTP_CONNECT_TIMEOUT)
            .timeout_read(SVN_HTTP_IO_TIMEOUT)
            .timeout_write(SVN_HTTP_IO_TIMEOUT)
            .redirects(0)
            .build();
        let mut session = Self {
            agent,
            base_url: base_url.clone(),
            authorization,
            revision_root_stub: base_url,
            browse_root_segments: Vec::new(),
            initial_youngest_revision: None,
        };
        let discovery = session.discover()?;
        session.apply_discovery(discovery)?;
        Ok(session)
    }

    /// 返回仓库最新修订号；首次调用复用建连 OPTIONS 结果，刷新时重新查询服务端。
    pub(super) fn latest_revision(&mut self) -> Result<u64> {
        if let Some(revision) = self.initial_youngest_revision.take() {
            return Ok(revision);
        }
        let discovery = self.discover()?;
        let revision = discovery.youngest_revision;
        self.apply_discovery(discovery)?;
        // 新发现结果已经通过 `apply_discovery` 写入缓存；本次结果已直接返回，避免下次重复消费。
        self.initial_youngest_revision = None;
        Ok(revision)
    }

    /// 枚举链接浏览根内指定目录的直接子项。
    ///
    /// 参数：`path` 是以链接位置为根的 UI 绝对路径，`revision` 是具体非负修订号。
    /// 返回值：目录优先、名称稳定排序的通用远程文件条目。
    pub(super) fn list_directory(&self, path: &str, revision: u64) -> Result<Vec<RemoteFileEntry>> {
        let normalized_path = normalize_browse_path(path)?;
        let request_url = self.revision_url(&normalized_path, revision)?;
        let (response, response_url) = self
            .send_request(
                "PROPFIND",
                &request_url,
                Some("1"),
                Some(SVN_DAV_PROPFIND_BODY),
                false,
            )?
            .expect("非可选 DAV 请求必须返回响应");
        let response = require_status(response, &[200, 207], "读取 SVN HTTP 目录")?;
        let xml = read_xml_response(response)?;
        let resources = parse_multistatus(&xml, &response_url)?;
        directory_entries_from_resources(
            resources,
            &response_url,
            &normalized_path,
            &self.browse_root_segments,
        )
    }

    /// 查询指定历史修订中的节点类型与大小；资源不存在时返回 `None`。
    pub(super) fn stat(&self, path: &str, revision: u64) -> Result<Option<HttpSvnNodeStat>> {
        let normalized_path = normalize_browse_path(path)?;
        let request_url = self.revision_url(&normalized_path, revision)?;
        let Some((response, response_url)) = self.send_request(
            "PROPFIND",
            &request_url,
            Some("0"),
            Some(SVN_DAV_PROPFIND_BODY),
            true,
        )?
        else {
            return Ok(None);
        };
        let response = require_status(response, &[200, 207], "读取 SVN HTTP 文件信息")?;
        let xml = read_xml_response(response)?;
        let resources = parse_multistatus(&xml, &response_url)?;
        // Depth=0 按 DAV 语义只返回目标资源；优先按最终响应 URL 精确匹配，兼容服务端
        // 把 `!svn/rvr` 重写为 `!svn/bc` 等历史资源别名时再取唯一响应项。
        let resource = resources
            .iter()
            .find(|resource| same_resource(&resource.url, &response_url))
            .or_else(|| (resources.len() == 1).then(|| &resources[0]));
        Ok(resource.map(|resource| HttpSvnNodeStat {
            kind: resource.kind,
            size: resource.size,
        }))
    }

    /// 读取历史修订中的文件前 `max_bytes` 字节，供受限预览使用。
    pub(super) fn read_file_bytes(
        &self,
        path: &str,
        revision: u64,
        max_bytes: u64,
    ) -> Result<Vec<u8>> {
        let normalized_path = normalize_browse_path(path)?;
        let request_url = self.revision_url(&normalized_path, revision)?;
        let response = self
            .send_request("GET", &request_url, None, None, false)?
            .expect("非可选 DAV 请求必须返回响应")
            .0;
        let response = require_status(response, &[200], "读取 SVN HTTP 文件")?;
        let mut reader = response.into_reader().take(max_bytes);
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .context("读取 SVN HTTP 文件响应失败")?;
        Ok(bytes)
    }

    /// 把历史修订中的文件流式写入本地路径，不把完整文件保存在内存中。
    pub(super) fn download_file(&self, path: &str, revision: u64, local_path: &Path) -> Result<()> {
        let normalized_path = normalize_browse_path(path)?;
        let request_url = self.revision_url(&normalized_path, revision)?;
        let response = self
            .send_request("GET", &request_url, None, None, false)?
            .expect("非可选 DAV 请求必须返回响应")
            .0;
        let response = require_status(response, &[200], "下载 SVN HTTP 文件")?;
        let mut local = File::create(local_path)
            .with_context(|| format!("无法创建本地文件 {}", local_path.display()))?;
        std::io::copy(&mut response.into_reader(), &mut local)
            .context("写入 SVN HTTP 下载文件失败")?;
        local.flush().context("刷新 SVN HTTP 下载文件失败")?;
        Ok(())
    }

    /// 发送 OPTIONS 并解析 Subversion HTTPv2 必需的修订资源头。
    fn discover(&self) -> Result<SvnDavDiscovery> {
        let (response, request_url) = self
            .send_request(
                "OPTIONS",
                &self.base_url,
                None,
                Some(SVN_DAV_OPTIONS_BODY),
                false,
            )?
            .expect("非可选 DAV 请求必须返回响应");
        let response = require_status(response, &[200], "发现 SVN HTTP 仓库")?;
        let youngest_revision = response
            .header("SVN-Youngest-Rev")
            .ok_or_else(|| anyhow!("服务端未返回 SVN-Youngest-Rev，URL 可能不是 SVN HTTP 仓库"))?
            .parse::<u64>()
            .context("服务端返回了无效的 SVN HEAD 修订号")?;
        let repository_root = response
            .header("SVN-Repository-Root")
            .ok_or_else(|| anyhow!("SVN HTTP 服务端未返回仓库根路径"))
            .and_then(|value| resolve_server_uri(&request_url, value))?;
        let revision_root_stub = response
            .header("SVN-Rev-Root-Stub")
            .ok_or_else(|| {
                anyhow!("SVN HTTP 服务端不支持 HTTPv2 修订资源；需要 Subversion 1.7 或更高版本")
            })
            .and_then(|value| resolve_server_uri(&request_url, value))?;
        ensure_same_origin(&request_url, &repository_root)?;
        ensure_same_origin(&request_url, &revision_root_stub)?;
        Ok(SvnDavDiscovery {
            request_url,
            youngest_revision,
            repository_root,
            revision_root_stub,
        })
    }

    /// 应用一次 OPTIONS 发现结果，并重新计算配置 URL 对应的不可越过浏览根。
    fn apply_discovery(&mut self, discovery: SvnDavDiscovery) -> Result<()> {
        let configured_segments = decoded_path_segments(&discovery.request_url)?;
        let repository_segments = decoded_path_segments(&discovery.repository_root)?;
        if !configured_segments.starts_with(&repository_segments) {
            bail!("SVN HTTP URL 不在服务端声明的仓库根路径内");
        }
        self.base_url = discovery.request_url;
        self.revision_root_stub = discovery.revision_root_stub;
        self.browse_root_segments = configured_segments[repository_segments.len()..].to_vec();
        self.initial_youngest_revision = Some(discovery.youngest_revision);
        Ok(())
    }

    /// 构造 HTTPv2 历史修订资源 URL，始终在配置链接指向的浏览根之下追加路径。
    fn revision_url(&self, browse_path: &str, revision: u64) -> Result<Url> {
        let mut url = self.revision_root_stub.clone();
        url.set_query(None);
        url.set_fragment(None);
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow!("SVN HTTP 修订根 URL 不能追加路径"))?;
        segments.pop_if_empty();
        segments.push(&revision.to_string());
        for segment in &self.browse_root_segments {
            segments.push(segment);
        }
        for segment in browse_path.trim_matches('/').split('/') {
            if !segment.is_empty() {
                segments.push(segment);
            }
        }
        drop(segments);
        Ok(url)
    }

    /// 发送带可选 DAV 深度和 XML 请求体的 HTTP 请求，并只跟随安全的同源重定向。
    ///
    /// `allow_not_found` 仅供 stat 使用；为真时 HTTP 404 映射为 `None`，其他错误仍返回错误。
    fn send_request(
        &self,
        method: &str,
        initial_url: &Url,
        depth: Option<&str>,
        body: Option<&str>,
        allow_not_found: bool,
    ) -> Result<Option<(ureq::Response, Url)>> {
        let mut request_url = initial_url.clone();
        for redirect_count in 0..=SVN_HTTP_MAX_REDIRECTS {
            let mut request = self
                .agent
                .request(method, request_url.as_str())
                .set("User-Agent", "Argus-SVN/0.1")
                .set("Accept", "*/*");
            if let Some(authorization) = &self.authorization {
                request = request.set("Authorization", authorization);
            }
            if let Some(depth) = depth {
                request = request.set("Depth", depth);
            }
            // OPTIONS 和 PROPFIND 都携带 XML；即使 OPTIONS 没有 Depth 头也必须声明媒体类型，
            // 否则部分 mod_dav_svn 会按普通空 OPTIONS 处理，不返回 SVN HTTPv2 专用响应头。
            if body.is_some() {
                request = request.set("Content-Type", "application/xml; charset=utf-8");
            }
            let response = match body {
                Some(body) => request.send_string(body),
                None => request.call(),
            };
            let response = match response {
                Ok(response) => response,
                Err(ureq::Error::Status(404, _)) if allow_not_found => return Ok(None),
                Err(ureq::Error::Status(status, response)) => {
                    return Err(http_status_error(
                        status,
                        &response,
                        self.authorization.is_some(),
                    ));
                }
                Err(ureq::Error::Transport(error)) => {
                    return Err(anyhow!("SVN HTTP 网络请求失败：{error}"));
                }
            };

            if !matches!(response.status(), 301 | 302 | 307 | 308) {
                return Ok(Some((response, request_url)));
            }
            if redirect_count == SVN_HTTP_MAX_REDIRECTS {
                bail!("SVN HTTP 重定向次数过多");
            }
            let location = response
                .header("Location")
                .ok_or_else(|| anyhow!("SVN HTTP 重定向缺少 Location"))?;
            let next_url = request_url
                .join(location)
                .context("SVN HTTP 重定向地址无效")?;
            ensure_same_origin(&request_url, &next_url)?;
            if request_url.scheme() == "https" && next_url.scheme() != "https" {
                bail!("已阻止 SVN HTTPS 向明文 HTTP 降级重定向");
            }
            if !next_url.username().is_empty() || next_url.password().is_some() {
                bail!("已阻止携带 URL 凭据的 SVN HTTP 重定向");
            }
            request_url = next_url;
        }
        unreachable!("重定向循环总会返回响应或错误")
    }
}

/// 从 SVN HTTP 配置生成无用户信息的请求 URL 和只驻留内存的 Basic 请求头。
fn sanitized_url_and_authorization(config: &SvnLinkConfig) -> Result<(Url, Option<String>)> {
    let mut base_url = Url::parse(&config.url).context("SVN HTTP URL 无效")?;
    if !matches!(base_url.scheme(), "http" | "https") {
        bail!("当前 URL 不是 HTTP(S) SVN 仓库");
    }
    let embedded_username = decode_url_component(base_url.username())?;
    let username = config.username.clone().or(embedded_username);
    let authorization = match (username, config.password.as_deref()) {
        (Some(username), Some(password)) => Some(format!(
            "Basic {}",
            STANDARD.encode(format!("{username}:{password}"))
        )),
        (None, None) => None,
        _ => bail!("SVN HTTP 用户名和密码必须成对配置"),
    };

    // 请求 URL 永远不携带凭据，避免网络错误、代理日志或重定向位置泄漏秘密。
    base_url
        .set_username("")
        .map_err(|_| anyhow!("无法清理 SVN HTTP URL 用户名"))?;
    base_url
        .set_password(None)
        .map_err(|_| anyhow!("无法清理 SVN HTTP URL 密码"))?;
    base_url.set_query(None);
    base_url.set_fragment(None);
    Ok((base_url, authorization))
}

/// 把 HTTP 状态转换为不包含密码、Authorization 或响应正文的用户提示。
fn http_status_error(
    status: u16,
    response: &ureq::Response,
    had_credentials: bool,
) -> anyhow::Error {
    match status {
        401 if had_credentials => anyhow!("SVN HTTP 认证失败，请检查用户名、密码及服务端认证方式"),
        401 => anyhow!("SVN HTTP 仓库需要用户名和密码"),
        403 => anyhow!("当前账号无权读取 SVN HTTP 仓库"),
        404 => anyhow!("SVN HTTP 仓库路径或修订不存在"),
        405 | 501 => anyhow!("服务器或代理未开放 SVN 所需的 OPTIONS/PROPFIND/GET 方法"),
        _ => anyhow!("SVN HTTP 服务返回 {status} {}", response.status_text()),
    }
}

/// 校验响应状态是否属于当前 DAV 操作接受的集合。
fn require_status(
    response: ureq::Response,
    accepted: &[u16],
    operation: &str,
) -> Result<ureq::Response> {
    if accepted.contains(&response.status()) {
        Ok(response)
    } else {
        bail!(
            "{operation}失败：HTTP {} {}",
            response.status(),
            response.status_text()
        )
    }
}

/// 有界读取 DAV XML 响应，超限时拒绝解析。
fn read_xml_response(response: ureq::Response) -> Result<String> {
    let mut reader = response
        .into_reader()
        .take(SVN_HTTP_MAX_XML_BYTES.saturating_add(1));
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .context("读取 SVN DAV XML 响应失败")?;
    if bytes.len() as u64 > SVN_HTTP_MAX_XML_BYTES {
        bail!("SVN DAV 目录响应超过 16 MiB，已停止解析");
    }
    String::from_utf8(bytes).context("SVN DAV 响应不是有效 UTF-8 XML")
}

/// 解析 DAV multistatus 中所有成功资源；404 propstat 不会伪装成有效条目。
fn parse_multistatus(xml: &str, request_url: &Url) -> Result<Vec<DavResource>> {
    let document = Document::parse(xml).context("无法解析 SVN DAV XML 响应")?;
    let mut resources = Vec::new();
    for response in document
        .descendants()
        .filter(|node| is_dav_element(*node, "response"))
    {
        let Some(href) = response
            .children()
            .find(|node| is_dav_element(*node, "href"))
            .and_then(|node| node.text())
        else {
            continue;
        };
        let Some(prop) = successful_dav_prop(response) else {
            continue;
        };
        let resource_url = resolve_server_uri(request_url, href.trim())?;
        ensure_same_origin(request_url, &resource_url)?;
        let display_name = dav_property_text(prop, "displayname")
            .filter(|name| !name.is_empty())
            .map(ToString::to_string)
            .or_else(|| decoded_last_path_segment(&resource_url).ok().flatten())
            .unwrap_or_default();
        let kind = if dav_property(prop, "resourcetype").is_some_and(|resource_type| {
            resource_type
                .descendants()
                .any(|node| is_dav_element(node, "collection"))
        }) {
            RemoteFileEntryKind::Directory
        } else {
            RemoteFileEntryKind::RegularFile
        };
        let size = dav_property_text(prop, "getcontentlength")
            .and_then(|value| value.trim().parse::<u64>().ok());
        let mtime = dav_property_text(prop, "getlastmodified")
            .and_then(parse_dav_timestamp)
            .or_else(|| dav_property_text(prop, "creationdate").and_then(parse_dav_timestamp));
        resources.push(DavResource {
            url: resource_url,
            name: display_name,
            kind,
            size,
            mtime,
        });
    }
    if resources.is_empty() {
        bail!("SVN DAV 响应未包含可读取的资源属性");
    }
    Ok(resources)
}

/// 把 Depth=1 DAV 资源转换为目录条目，并排除响应中必然包含的目标目录自身。
///
/// 部分 SVN 服务端会在响应中把请求使用的 `!svn/rvr` 改写为 `!svn/bc` 等等价历史资源，
/// 因而不能只比较完整 URL。这里先尝试最终请求 URL，再以仓库逻辑路径后缀和最短路径识别
/// 目标目录；同名真实子目录比目标目录多一个路径段，不会被误删。
fn directory_entries_from_resources(
    resources: Vec<DavResource>,
    request_url: &Url,
    normalized_path: &str,
    browse_root_segments: &[String],
) -> Result<Vec<RemoteFileEntry>> {
    let current_directory_index = current_directory_resource_index(
        &resources,
        request_url,
        normalized_path,
        browse_root_segments,
    )?;
    let mut entries = resources
        .into_iter()
        .enumerate()
        .filter(|(index, _)| *index != current_directory_index)
        .map(|(_, resource)| {
            validate_entry_name(&resource.name)?;
            Ok(RemoteFileEntry {
                path: remote_child_path(normalized_path, &resource.name),
                name: resource.name,
                kind: resource.kind,
                size: resource.size,
                mtime: resource.mtime,
                permissions: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    entries.sort_by_cached_key(|entry| {
        (
            entry.kind != RemoteFileEntryKind::Directory,
            entry.name.to_ascii_lowercase(),
            entry.name.clone(),
        )
    });
    Ok(entries)
}

/// 定位 Depth=1 multistatus 中代表当前目录的响应项。
///
/// 参数：`normalized_path` 以链接位置为根，`browse_root_segments` 是链接位置相对仓库根的路径。
/// 返回值：当前目录在资源数组中的下标；服务端缺少目标目录响应时返回协议错误。
fn current_directory_resource_index(
    resources: &[DavResource],
    request_url: &Url,
    normalized_path: &str,
    browse_root_segments: &[String],
) -> Result<usize> {
    if let Some(index) = resources
        .iter()
        .position(|resource| same_resource(&resource.url, request_url))
    {
        if resources[index].kind != RemoteFileEntryKind::Directory {
            bail!("SVN DAV 把当前目录报告为非目录资源");
        }
        return Ok(index);
    }

    let mut logical_segments = browse_root_segments.to_vec();
    logical_segments.extend(
        normalized_path
            .trim_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(ToString::to_string),
    );
    let mut candidates = resources
        .iter()
        .enumerate()
        .filter_map(|(index, resource)| {
            if resource.kind != RemoteFileEntryKind::Directory {
                return None;
            }
            let segments = decoded_path_segments(&resource.url).ok()?;
            if !logical_segments.is_empty() && !segments.ends_with(&logical_segments) {
                return None;
            }
            Some((index, segments.len()))
        })
        .collect::<Vec<_>>();
    // 当前目录与同名子目录都可能以相同逻辑后缀结尾；当前目录的规范 URL 路径必然更短。
    candidates.sort_by_key(|(_, segment_count)| *segment_count);
    candidates
        .first()
        .map(|(index, _)| *index)
        .ok_or_else(|| anyhow!("SVN DAV 目录响应未包含当前目录资源"))
}

/// 返回 response 中 HTTP 200 propstat 下的 DAV:prop 节点。
fn successful_dav_prop<'a, 'input>(response: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    response
        .children()
        .filter(|node| is_dav_element(*node, "propstat"))
        .find_map(|propstat| {
            let is_success = propstat
                .children()
                .find(|node| is_dav_element(*node, "status"))
                .and_then(|node| node.text())
                .is_some_and(|status| status.split_ascii_whitespace().any(|part| part == "200"));
            is_success.then(|| {
                propstat
                    .children()
                    .find(|node| is_dav_element(*node, "prop"))
            })?
        })
}

/// 返回 DAV:prop 下指定属性节点。
fn dav_property<'a, 'input>(prop: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    prop.children().find(|node| is_dav_element(*node, name))
}

/// 返回 DAV:prop 下指定属性的首段文本。
fn dav_property_text<'a, 'input>(prop: Node<'a, 'input>, name: &str) -> Option<&'a str> {
    dav_property(prop, name)
        .and_then(|node| node.text())
        .map(str::trim)
}

/// 判断 XML 节点是否属于 DAV 命名空间且本地名称匹配。
fn is_dav_element(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element()
        && node.tag_name().namespace() == Some("DAV:")
        && node.tag_name().name() == name
}

/// 解析 DAV RFC2822/RFC3339 时间并拒绝负 Unix 时间戳。
fn parse_dav_timestamp(value: &str) -> Option<u64> {
    DateTime::parse_from_rfc2822(value)
        .or_else(|_| DateTime::parse_from_rfc3339(value))
        .ok()
        .and_then(|time| u64::try_from(time.timestamp()).ok())
}

/// 解析服务端返回的绝对或相对 URI。
fn resolve_server_uri(base_url: &Url, value: &str) -> Result<Url> {
    Url::parse(value)
        .or_else(|_| base_url.join(value))
        .context("SVN DAV 服务端返回了无效 URI")
}

/// 校验两个 URL 同源，避免 Basic 凭据或仓库内容被重定向到其他主机。
fn ensure_same_origin(base_url: &Url, target_url: &Url) -> Result<()> {
    let same_origin = base_url.scheme() == target_url.scheme()
        && base_url.host_str() == target_url.host_str()
        && base_url.port_or_known_default() == target_url.port_or_known_default();
    if !same_origin {
        bail!("已阻止 SVN HTTP 跨源资源或重定向");
    }
    Ok(())
}

/// 比较两个 DAV 资源 URL，目录末尾斜杠及等价百分号编码不影响资源身份。
fn same_resource(left: &Url, right: &Url) -> bool {
    let same_origin = left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default();
    if !same_origin {
        return false;
    }
    left.path().trim_end_matches('/') == right.path().trim_end_matches('/')
        || decoded_path_segments(left)
            .ok()
            .zip(decoded_path_segments(right).ok())
            .is_some_and(|(left, right)| left == right)
}

/// 把 URL 路径段解码成 UTF-8，用于计算配置 URL 相对仓库根的路径。
fn decoded_path_segments(url: &Url) -> Result<Vec<String>> {
    url.path_segments()
        .ok_or_else(|| anyhow!("SVN HTTP URL 不能作为层级路径"))?
        .filter(|segment| !segment.is_empty())
        .map(|segment| decode_url_component(segment).map(Option::unwrap_or_default))
        .collect()
}

/// 解码 URL 最后一段，供缺少 DAV:displayname 的服务端回退使用。
fn decoded_last_path_segment(url: &Url) -> Result<Option<String>> {
    url.path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .map(decode_url_component)
        .transpose()
        .map(Option::flatten)
}

/// 解码单个百分号编码 URL 组件；空组件返回 `None`。
fn decode_url_component(value: &str) -> Result<Option<String>> {
    if value.is_empty() {
        return Ok(None);
    }
    percent_decode_str(value)
        .decode_utf8()
        .map(|value| Some(value.into_owned()))
        .context("SVN HTTP URL 包含非 UTF-8 路径或用户名")
}

/// 归一化以链接位置为根的 UI 路径，并拒绝任何父目录或反斜杠逃逸。
fn normalize_browse_path(path: &str) -> Result<String> {
    let mut components = Vec::new();
    for component in path.trim().split('/') {
        match component {
            "" | "." => {}
            ".." => bail!("仓库路径不能越过浏览根目录"),
            value if value.contains('\\') => bail!("仓库路径不能包含反斜杠"),
            value => components.push(value),
        }
    }
    Ok(if components.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", components.join("/"))
    })
}

/// 校验服务端目录项名称只能是一个安全路径组件。
fn validate_entry_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        bail!("SVN DAV 服务端返回了不安全的目录项名称");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 返回包含根目录、一个子目录和一个普通文件的 DAV multistatus。
    fn directory_multistatus() -> &'static str {
        r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response><D:href>http://example.com/svn/repo/!svn/rvr/42/</D:href><D:propstat><D:prop><D:displayname>repo</D:displayname><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
  <D:response><D:href>/svn/repo/!svn/rvr/42/src/</D:href><D:propstat><D:prop><D:displayname>src</D:displayname><D:resourcetype><D:collection/></D:resourcetype><D:getlastmodified>Wed, 21 Oct 2015 07:28:00 GMT</D:getlastmodified></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
  <D:response><D:href>/svn/repo/!svn/rvr/42/README.md</D:href><D:propstat><D:prop><D:displayname>README.md</D:displayname><D:resourcetype/><D:getcontentlength>9</D:getcontentlength><D:creationdate>2026-07-15T08:00:00Z</D:creationdate></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
</D:multistatus>"#
    }

    /// 验证 DAV multistatus 能区分自身、目录和普通文件，并解析大小与时间。
    #[test]
    fn dav_multistatus_parses_directory_and_file_metadata() {
        let request_url = Url::parse("http://example.com/svn/repo/!svn/rvr/42/").unwrap();
        let resources = parse_multistatus(directory_multistatus(), &request_url).unwrap();
        assert_eq!(resources.len(), 3);
        assert!(same_resource(&resources[0].url, &request_url));
        assert_eq!(resources[1].name, "src");
        assert_eq!(resources[1].kind, RemoteFileEntryKind::Directory);
        assert!(resources[1].mtime.is_some());
        assert_eq!(resources[2].name, "README.md");
        assert_eq!(resources[2].kind, RemoteFileEntryKind::RegularFile);
        assert_eq!(resources[2].size, Some(9));
    }

    /// 验证服务端把 rvr 请求改写成 bc 历史资源时，当前目录不会作为同名子目录显示。
    #[test]
    fn directory_listing_filters_current_directory_with_different_dav_alias() {
        let request_url = Url::parse("http://example.com/svn/repo/!svn/rvr/16/src").unwrap();
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response><D:href>/svn/repo/!svn/bc/16/src/</D:href><D:propstat><D:prop><D:displayname>src</D:displayname><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
  <D:response><D:href>/svn/repo/!svn/bc/16/src/com/</D:href><D:propstat><D:prop><D:displayname>com</D:displayname><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
</D:multistatus>"#;
        let resources = parse_multistatus(xml, &request_url).unwrap();
        let entries =
            directory_entries_from_resources(resources, &request_url, "/src", &[]).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "com");
        assert_eq!(entries[0].path, "/src/com");
    }

    /// 验证 revision URL 把链接子目录固定为浏览根，并正确编码 Unicode 路径段。
    #[test]
    fn revision_url_preserves_configured_browse_root_and_encodes_path() {
        let session = HttpSvnSession {
            agent: ureq::agent(),
            base_url: Url::parse("http://example.com/svn/repo/trunk/").unwrap(),
            authorization: None,
            revision_root_stub: Url::parse("http://example.com/svn/repo/!svn/rvr").unwrap(),
            browse_root_segments: vec!["trunk".to_string()],
            initial_youngest_revision: None,
        };
        assert_eq!(
            session.revision_url("/目录/文件.txt", 7).unwrap().as_str(),
            "http://example.com/svn/repo/!svn/rvr/7/trunk/%E7%9B%AE%E5%BD%95/%E6%96%87%E4%BB%B6.txt"
        );
        assert!(normalize_browse_path("/../secret").is_err());
    }

    /// 验证 HTTP 用户名从 URL 或独立字段读取，最终请求 URL 不保留任何凭据信息。
    #[test]
    fn http_credentials_are_encoded_in_memory_and_removed_from_request_url() {
        let (url, authorization) = sanitized_url_and_authorization(&SvnLinkConfig {
            url: "http://reader@example.com/svn/repo/".to_string(),
            password: Some("secret".to_string()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(url.as_str(), "http://example.com/svn/repo/");
        assert_eq!(authorization.as_deref(), Some("Basic cmVhZGVyOnNlY3JldA=="));
        assert!(!format!("{url:?}").contains("secret"));
    }
}
