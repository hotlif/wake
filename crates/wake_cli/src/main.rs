//! Wake CLI 入口（bin: `wake`）。
//!
//! Phase 0：命令骨架（`build` / `dev` / `parse` / `tokenize`）。`build <entry>` 能读文件
//! 并渲染一条带源码上下文的诊断，验证 wake_common 的诊断链路（PLAN §0.3 / Gate-0）。
//! 真正的编译/打包在 P1+ 逐步接入。

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use wake_common::{FileSystem, OsFileSystem, RenderStyle, SourceFile, render};

mod ui;
use ui::{Ui, human_bytes, human_dur};

#[derive(Parser)]
#[command(name = "wake", version, about = "高性能 Rust Web 构建器", long_about = None)]
struct Cli {
    /// 强制关闭彩色输出（也遵循环境变量 NO_COLOR）。
    #[arg(long, global = true)]
    no_color: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 构建应用（读 `wake.config.toml`、组件扫描、别名、产出 dist + index.html）。
    Build {
        /// 入口文件路径。省略则由配置驱动：生成虚拟入口 `import("@/entry.tsx")`（对齐 crustify `app:build`）。
        entry: Option<PathBuf>,
        /// 输出目录。
        #[arg(long, default_value = "dist")]
        outdir: PathBuf,
        /// 启用持久化构建缓存（`.wake/cache.bin`）：全新进程冷构建跳过未变模块的 parse+codegen（PLAN §7.1）。
        #[arg(long)]
        cache: bool,
        /// 监听源码变更，进程常驻热重建（引擎保持温热，增量重建远快于每次冷起）。
        #[arg(long)]
        watch: bool,
        /// 产出 Source Map（`<chunk>.js.map` + `sourceMappingURL`）。
        ///
        /// 注意：当前仅**非压缩**产物支持精确映射，故本选项会关闭 minify/mangle
        /// （压缩路径会重排改写模块体，映射会错位）。用于调试生产构建的模块组合问题。
        #[arg(long)]
        sourcemap: bool,
    },
    /// 启动 Dev Server + HMR（Phase 5，actix-web）。
    Dev {
        /// 项目根目录。
        #[arg(default_value = ".")]
        root: PathBuf,
        /// 监听端口。
        #[arg(long, default_value_t = 5173)]
        port: u16,
    },
    /// 解析并打印 AST（Phase 2）。
    Parse {
        /// 源文件路径。
        file: PathBuf,
        /// 以 JSON 输出 AST。
        #[arg(long)]
        ast: bool,
    },
    /// 词法分析并打印 token 流（Phase 1）。
    Tokenize {
        /// 源文件路径。
        file: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let style = resolve_style(cli.no_color);

    let result = match cli.command {
        Command::Build {
            entry,
            outdir,
            cache,
            watch,
            sourcemap,
        } => {
            if watch {
                cmd_build_watch(entry.as_deref(), &outdir, sourcemap, Ui::new(style.color))
            } else {
                cmd_build(
                    entry.as_deref(),
                    &outdir,
                    cache,
                    sourcemap,
                    Ui::new(style.color),
                )
            }
        }
        Command::Dev { root, port } => cmd_dev(&root, port),
        Command::Parse { file, ast } => cmd_parse(&file, ast, style),
        Command::Tokenize { file } => cmd_tokenize(&file, style),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

/// 读 `wake.config.toml` + 运行组件扫描（写 `.wake/scan/{ns}.ts`）+ 组装别名（`@`/`@@`/配置项/`@@@/{ns}`）。
///
/// 配置缺失时回退默认（零配置可跑）；解析失败打印告警并用默认。返回 (配置, 项目根, 别名表)。
/// CRUSTIFY-PARITY §M1+§M2：别名解析 + 组件自动扫描（`@@@/{ns}` 懒加载模块）。
fn prepare_project(start_dir: &Path) -> (wake_config::Config, PathBuf, Vec<(String, PathBuf)>) {
    // `config_dir` = 配置文件所在目录（向上探测得到）；项目根可由 `root_dir` 覆盖。
    let config_dir = wake_config::find_root(start_dir);
    let config = wake_config::load(&config_dir).unwrap_or_else(|e| {
        eprintln!("warning: {e}（改用默认配置）");
        wake_config::Config::default()
    });
    // `root_dir` 相对配置文件目录解析（绝对路径则原样取用），此后**一切基准都用它**：
    // 别名 `@`→root/src、`@@`→root、组件扫描的 cwd、`.wake/` 目录、虚拟入口、HTML 模板。
    // 对齐 crustify 的 `getCwdDir(conf.rootDir)`。
    let root = normalize_root(config.resolved_root(&config_dir));
    if root != config_dir && !root.is_dir() {
        // `find_root` 对相对入口可能返回空路径（一路向上走到 `""`），直接 display 会是空串。
        let shown = |p: &Path| {
            if p.as_os_str().is_empty() {
                ".".to_string()
            } else {
                p.display().to_string()
            }
        };
        eprintln!(
            "warning: 配置的 root_dir 指向不存在的目录 `{}`（回退到 `{}`）",
            shown(&root),
            shown(&config_dir)
        );
        return prepare_with_root(config, config_dir);
    }
    prepare_with_root(config, root)
}

/// 去掉 `join(".")` 之类留下的 `.` 分量，使日志/别名里的路径可读。
fn normalize_root(p: PathBuf) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

fn prepare_with_root(
    config: wake_config::Config,
    root: PathBuf,
) -> (wake_config::Config, PathBuf, Vec<(String, PathBuf)>) {
    let mut aliases = config.resolver_aliases(&root);

    // 组件自动扫描：每条规则生成 `@@@/{ns}` 懒加载模块，写入 `.wake/scan/{ns}.ts` 并登记别名。
    if !config.component_scan.is_empty() {
        let scan_base = root.join(".wake").join("scan");
        let _ = std::fs::create_dir_all(&scan_base);
        for rule in &config.component_scan {
            let scan_abs = root.join(&rule.cwd);
            let module = wake_scan::scan(&wake_scan::ScanRule {
                namespace: &rule.namespace,
                scan_dir: &scan_abs,
                root: &root,
                generate_source: rule.generate_source,
                include: rule.include.as_deref(),
                exclude: rule.exclude.as_deref(),
            });
            match module {
                Ok(src) => {
                    let file = scan_base.join(format!("{}.ts", sanitize_ns(&rule.namespace)));
                    if let Err(e) = std::fs::write(&file, src) {
                        eprintln!("warning: 无法写入扫描模块 `{}`：{e}", file.display());
                        continue;
                    }
                    aliases.push((format!("@@@/{}", rule.namespace), file));
                }
                Err(e) => eprintln!("warning: 组件扫描 `{}` 失败：{e}", rule.namespace),
            }
        }
    }

    (config, root, aliases)
}

/// 清空输出目录（对齐 crustify `output.clean`）：移除旧产物（含过期 hash chunk），随后写盘重建。
/// 安全护栏：仅在目录存在且有文件名（非根 / 非 `.`）时删除。
fn clean_outdir(outdir: &Path) {
    if outdir.exists() && outdir.file_name().is_some() {
        let _ = std::fs::remove_dir_all(outdir);
    }
}

/// 命名空间 → 安全文件名（非字母数字 → `_`）。
fn sanitize_ns(ns: &str) -> String {
    ns.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// 别名表 → `ResolveOptions`。
fn resolve_options(aliases: Vec<(String, PathBuf)>) -> wake_bundler::ResolveOptions {
    wake_bundler::ResolveOptions {
        alias: aliases,
        ..wake_bundler::ResolveOptions::default()
    }
}

/// 组装编译期 define：`process.env.NODE_ENV`（prod=`"production"` / dev=`"development"`）+ 用户 `[define]`
/// （用户可覆盖 NODE_ENV）。值为字面量**源码**（字符串须自带引号）。CRUSTIFY-PARITY §M3。
fn build_define(config: &wake_config::Config, dev: bool) -> Vec<(String, String)> {
    let node_env = if dev {
        "\"development\""
    } else {
        "\"production\""
    };
    let mut v: Vec<(String, String)> =
        vec![("process.env.NODE_ENV".to_string(), node_env.to_string())];
    for (k, val) in &config.define {
        if let Some(slot) = v.iter_mut().find(|(kk, _)| kk == k) {
            slot.1 = val.clone();
        } else {
            v.push((k.clone(), val.clone()));
        }
    }
    v
}

/// 生成虚拟入口 `.wake/entry.tsx`（`import` 配置入口，经 `@@` 别名），返回其路径。
/// 对齐 crustify 的 `.tmp/entry.tsx` = `import("@/entry.tsx")`（决策②/§M2）。
fn virtual_entry(root: &Path, config: &wake_config::Config) -> std::io::Result<PathBuf> {
    let target = config
        .html
        .entry
        .as_deref()
        .unwrap_or("src/entry.tsx")
        .replace('\\', "/");
    let dir = root.join(".wake");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("entry.tsx");
    std::fs::write(&path, format!("import(\"@@/{target}\");\n"))?;
    Ok(path)
}

/// 生成并写入 `dist/index.html`：注入入口 chunk 的 `<script defer>`（CSS `<link>` 待 M3 prod 抽取）。
/// 模板取 `config.html.template` / `public/index.html` / 内置默认外壳。
fn emit_html(
    out: &wake_bundler::BuildOutput,
    config: &wake_config::Config,
    root: &Path,
    outdir: &Path,
) {
    // 仅入口 chunk 需 `<script>`；async/shared chunk 由运行时按需加载。
    let scripts: Vec<String> = out
        .chunks
        .iter()
        .filter(|c| c.is_entry)
        .map(|c| c.file_name.clone())
        .collect();
    // 抽取的 `.css` 产物注入 `<link>`。
    let styles: Vec<String> = out
        .assets
        .iter()
        .filter(|a| a.is_css)
        .map(|a| a.file_name.clone())
        .collect();
    let tpl_path = config
        .html
        .template
        .as_deref()
        .map(|t| root.join(t))
        .unwrap_or_else(|| root.join("public/index.html"));
    let template = std::fs::read_to_string(&tpl_path).ok();
    let html = wake_html::generate(
        template.as_deref(),
        &wake_html::HtmlInputs {
            scripts: &scripts,
            styles: &styles,
            public_path: config.public_path(),
        },
    );
    if let Err(e) = write_atomic(&outdir.join("index.html"), html.as_bytes()) {
        eprintln!("warning: 无法写入 index.html：{e}");
    }
}

/// 是否着色：`--no-color` / `NO_COLOR` 环境变量优先，否则看 stderr 是否 tty。
fn resolve_style(no_color_flag: bool) -> RenderStyle {
    let disabled = no_color_flag || std::env::var_os("NO_COLOR").is_some();
    if disabled || !std::io::stderr().is_terminal() {
        RenderStyle::plain()
    } else {
        RenderStyle::colored()
    }
}

fn cmd_build(
    entry: Option<&Path>,
    outdir: &Path,
    cache: bool,
    sourcemap: bool,
    ui: Ui,
) -> Result<(), ExitCode> {
    use std::sync::Arc;
    use std::time::Instant;

    let timing = std::env::var_os("WAKE_TIMING").is_some();
    let started = Instant::now();
    print_banner(&ui, "build");

    // 读配置 + 组件扫描 + 别名（从入口目录 / cwd 向上找项目根）。
    let start_dir = entry
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let (config, root, aliases) = prepare_project(&start_dir);
    // 入口：显式给定则用之，否则由配置生成虚拟入口（对齐 crustify `app:build`）。
    let entry_path = match entry {
        Some(e) => e.to_path_buf(),
        None => match virtual_entry(&root, &config) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  {} 无法生成虚拟入口：{e}", ui.err("✗"));
                return Err(ExitCode::FAILURE);
            }
        },
    };

    let fs = Arc::new(OsFileSystem);
    // 用增量+并行打包器（IncrementalBundler）：它按扩展名选择源类型（.ts/.tsx → TS 擦除模式），
    // 是 Phase 3 引擎接入后的生产路径；旧 MVP `Bundler` 硬编码 SourceType::Module，不识别 TS。
    let mut bundler = wake_bundler::IncrementalBundler::new(fs);
    // 装配别名（@→src、@@→根、配置项、@@@/{ns}→扫描产物），须在首次 build 前设置。
    bundler.set_resolve_options(resolve_options(aliases));
    // prod define：`process.env.NODE_ENV → "production"` + 用户 `[define]`（React prod 关键）。
    bundler.set_define(build_define(&config, false));
    // prod 产物完善：CSS 抽取为独立 `.css`、资源 4KB 阈值（超阈值独立产物）、publicPath 前缀（§M3）。
    bundler.enable_css_extraction();
    // 零运行时 CSS-in-JS（Linaria 子集，§M5）：默认开启——项目没 import `@linaria/core`
    // 时打包器整体跳过，零开销。
    bundler.enable_css_in_js();
    bundler.set_asset_inline_limit(4096);
    bundler.set_public_path(config.public_path());
    // `--sourcemap`：映射仅在非压缩路径精确（压缩会重排改写模块体），故与 minify/mangle 互斥。
    if sourcemap {
        bundler.enable_sourcemap();
        eprintln!(
            "  {} --sourcemap 已启用：本次构建不压缩（压缩路径的映射会错位）",
            ui.warn("!")
        );
    } else {
        bundler.enable_minify();
        if std::env::var_os("WAKE_NO_MANGLE").is_none() {
            bundler.enable_mangle();
        }
    }
    bundler.enable_dead_module_elimination();
    // prod build 开启 Tree Shaking（移除未用导出，PLAN §6.6）+ 代码分割（动态 import 切 chunk，
    // PLAN §6.5）；dev（wake dev）保持关闭利于 HMR。
    bundler.enable_tree_shaking();
    // 代码分割路径暂不产 map（M4d 首期只覆盖单包）——开 sourcemap 时关闭分割以保证映射完整。
    if !sourcemap {
        bundler.enable_code_splitting();
    }
    // `--cache`：持久化构建缓存（PLAN §7.1）。跨进程跳过未变模块的 parse+codegen。
    if cache {
        let cache_dir = root.join(".wake");
        let _ = std::fs::create_dir_all(&cache_dir);
        bundler.enable_persistent_cache(cache_dir.join("cache.bin"));
    }
    // 清空 dist（移除过期 hash chunk），随后写盘重建（对齐 crustify `clean: true`）。
    clean_outdir(outdir);
    let t_construct = started.elapsed();
    let out = bundler.build(&entry_path);
    let t_build = started.elapsed();

    let errors = out.diagnostics.iter().filter(|d| d.is_error()).count();
    let warnings = out.diagnostics.len() - errors;

    if out.has_errors() {
        eprintln!(
            "  {}  {}  {}  {}",
            ui.err("✗"),
            ui.bold("构建失败"),
            ui.dim("·"),
            ui.err(&format!("{errors} 个错误"))
        );
        eprintln!();
        print_diagnostics(&ui, &out.diagnostics);
        return Err(ExitCode::FAILURE);
    }

    // 写盘：全部 chunk + manifest.json（原子写、免写未变）。
    let total_bytes = match write_build_output(&out, outdir) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("  {} {e}", ui.err("✗"));
            return Err(ExitCode::FAILURE);
        }
    };
    // 生成 dist/index.html（静态外壳 + 入口 chunk 注入，CRUSTIFY-PARITY §M2）。
    emit_html(&out, &config, &root, outdir);
    if timing {
        let t_write = started.elapsed();
        eprintln!(
            "[wake-cli-timing] 构造(含线程池/缓存载入)={:.1?} | build()={:.1?} | 写盘={:.1?} | 合计={:.1?}",
            t_construct,
            t_build - t_construct,
            t_write - t_build,
            t_write,
        );
    }
    let out_path = outdir.join(&out.entry().file_name);

    // ✓ 构建成功  ·  19 模块  ·  3 chunk  ·  Yarn PnP  ·  1.58 MB  ·  3.43s
    let sep = ui.dim("·");
    let chunk_note = if out.chunks.len() > 1 {
        format!("{} chunk  {sep}  ", out.chunks.len())
    } else {
        String::new()
    };
    // 检测到 Yarn PnP 项目时标注（依赖来自 .pnp.cjs + zip 缓存，非 node_modules）。
    let pnp_note = if bundler.is_pnp() {
        format!("{}  {sep}  ", ui.accent("Yarn PnP"))
    } else {
        String::new()
    };
    println!(
        "  {}  {}  {sep}  {}  {sep}  {chunk_note}{pnp_note}{}  {sep}  {}",
        ui.ok("✓"),
        ui.bold("构建成功"),
        ui.accent(&format!("{} 模块", out.module_count)),
        ui.accent(&human_bytes(total_bytes)),
        ui.accent(&human_dur(started.elapsed())),
    );
    println!(
        "    {} {}",
        ui.dim("→"),
        ui.dim(&out_path.display().to_string())
    );
    // 多产物时逐 chunk 列出。
    if out.chunks.len() > 1 {
        for c in &out.chunks {
            if c.is_entry {
                continue;
            }
            println!(
                "    {} {}  {}",
                ui.dim("·"),
                ui.dim(&c.file_name),
                ui.dim(&format!(
                    "[{}] {}",
                    c.kind.as_str(),
                    human_bytes(c.code.len())
                )),
            );
        }
    }
    if warnings > 0 {
        println!("    {}", ui.warn(&format!("{warnings} 个警告")));
        print_diagnostics(&ui, &out.diagnostics);
    }
    println!();
    let _ = std::io::stdout().flush();
    Ok(())
}

/// 原子写盘：先写同目录临时文件，再 rename 覆盖目标。
///
/// **为何不用 `fs::write` 直接覆盖**：Windows Defender 实时保护会在文件写完后异步扫描它
/// （1.58MB bundle ~2s）。若下一次构建立即 `CreateFile(CREATE_ALWAYS)` 截断这个正被扫描的
/// 文件，会阻塞到扫描结束（实测 ~2s）。而 rename 只替换目录项——Defender 用 share-delete
/// 打开旧文件，旧句柄被孤立、扫描继续、新文件立刻就位，不阻塞。这是 esbuild/Vite 等在
/// Windows 上普遍采用的写法。临时名带 pid 避免并发构建相撞；rename 失败则回退直接写。
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // 注意：**不要**先读旧文件比对来「免写未变」。**打开**刚写过的大文件会触发 Defender
    // 的 scan-on-access 同步扫描（1.58MB ~2s，实测 run2 写盘 2.2s）。rename 之所以不阻塞，
    // 正是因为它**不打开**目标、只换目录项。故这里只走 temp+rename，宁可每次都写。
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("out");
    let tmp = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // rename 失败（罕见）：清理临时文件后退回直接写，保证功能不回退。
            let _ = std::fs::remove_file(&tmp);
            std::fs::write(path, bytes).map_err(|_| e)
        }
    }
}

/// 写盘：创建 outdir + 所有 chunk + 带外产物（资源/CSS）+ manifest.json（原子写）。返回总字节。
fn write_build_output(out: &wake_bundler::BuildOutput, outdir: &Path) -> Result<usize, String> {
    std::fs::create_dir_all(outdir)
        .map_err(|e| format!("无法创建输出目录 `{}`：{e}", outdir.display()))?;
    let mut total = 0usize;
    for c in &out.chunks {
        let p = outdir.join(&c.file_name);
        match &c.source_map {
            // 有 map：产物末尾追加 sourceMappingURL（外链），并另写 `<chunk>.js.map`。
            Some(map) => {
                let code = format!("{}\n//# sourceMappingURL={}.map\n", c.code, c.file_name);
                write_atomic(&p, code.as_bytes())
                    .map_err(|e| format!("无法写入 `{}`：{e}", p.display()))?;
                total += code.len();
                let mp = outdir.join(format!("{}.map", c.file_name));
                write_atomic(&mp, map.as_bytes())
                    .map_err(|e| format!("无法写入 `{}`：{e}", mp.display()))?;
                total += map.len();
            }
            None => {
                write_atomic(&p, c.code.as_bytes())
                    .map_err(|e| format!("无法写入 `{}`：{e}", p.display()))?;
                total += c.code.len();
            }
        }
    }
    // 带外产物：超阈值资源文件 + 抽取的 `.css`。
    for a in &out.assets {
        let p = outdir.join(&a.file_name);
        write_atomic(&p, &a.bytes).map_err(|e| format!("无法写入 `{}`：{e}", p.display()))?;
        total += a.bytes.len();
    }
    let _ = write_atomic(
        &outdir.join("manifest.json"),
        build_manifest(out).as_bytes(),
    );
    Ok(total)
}

/// `wake build --watch`：进程常驻，引擎保持温热。首次冷构建后监听源码，改动即**增量**热重建
/// 并写盘。省掉每次冷起的进程启动 + 构造（线程池）+ 缓存载入——热重建远快于一次性 `wake build`。
fn cmd_build_watch(
    entry: Option<&Path>,
    outdir: &Path,
    sourcemap: bool,
    ui: Ui,
) -> Result<(), ExitCode> {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use notify::{RecursiveMode, Watcher};

    print_banner(&ui, "build --watch");

    // 读配置 + 组件扫描 + 别名 + 入口解析（扫描仅在启动时跑一次，M2 限制）。
    let start_dir = entry
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let (config, root, aliases) = prepare_project(&start_dir);
    let entry_path = match entry {
        Some(e) => e.to_path_buf(),
        None => match virtual_entry(&root, &config) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  {} 无法生成虚拟入口：{e}", ui.err("✗"));
                return Err(ExitCode::FAILURE);
            }
        },
    };

    // 温热打包器：**一次** 创建，跨重建复用（引擎的内容 cell 保留 → 只有改动模块重 parse）。
    let mut bundler = wake_bundler::IncrementalBundler::new(Arc::new(OsFileSystem));
    bundler.set_resolve_options(resolve_options(aliases));
    // `build --watch` 走 prod 口径（同 `build`）；dev 口径在 `wake dev`。
    bundler.set_define(build_define(&config, false));
    bundler.enable_css_extraction();
    // 零运行时 CSS-in-JS（Linaria 子集，§M5）：默认开启——项目没 import `@linaria/core`
    // 时打包器整体跳过，零开销。
    bundler.enable_css_in_js();
    bundler.set_asset_inline_limit(4096);
    bundler.set_public_path(config.public_path());
    // 同 `cmd_build`：`--sourcemap` 与压缩/分割互斥（压缩路径映射会错位，分割路径暂不产 map）。
    if sourcemap {
        bundler.enable_sourcemap();
        eprintln!(
            "  {} --sourcemap 已启用：本次构建不压缩（压缩路径的映射会错位）",
            ui.warn("!")
        );
    } else {
        bundler.enable_minify();
        if std::env::var_os("WAKE_NO_MANGLE").is_none() {
            bundler.enable_mangle();
        }
    }
    bundler.enable_dead_module_elimination();
    bundler.enable_tree_shaking();
    if !sourcemap {
        bundler.enable_code_splitting();
    }

    // 一次构建 + 写盘 + HTML + 打印一行状态（label 区分首次/热重建）。
    let run_once = |bundler: &mut wake_bundler::IncrementalBundler, label: &str| {
        let started = Instant::now();
        let out = bundler.build(&entry_path);
        if out.has_errors() {
            let n = out.diagnostics.iter().filter(|d| d.is_error()).count();
            eprintln!(
                "  {}  {}  {}",
                ui.err("✗"),
                ui.bold(label),
                ui.err(&format!("{n} 个错误"))
            );
            print_diagnostics(&ui, &out.diagnostics);
            return;
        }
        match write_build_output(&out, outdir) {
            Ok(bytes) => {
                emit_html(&out, &config, &root, outdir);
                println!(
                    "  {}  {}  {}  {}  {}  {}  {}  {}",
                    ui.ok("✓"),
                    ui.bold(label),
                    ui.dim("·"),
                    ui.accent(&format!("{} 模块", out.module_count)),
                    ui.dim("·"),
                    ui.accent(&human_bytes(bytes)),
                    ui.dim("·"),
                    ui.accent(&human_dur(started.elapsed())),
                );
            }
            Err(e) => eprintln!("  {} {e}", ui.err("✗")),
        }
        let _ = std::io::stdout().flush();
    };

    // 首次构建前清空 dist；后续热重建不清（避免删掉正被服务的产物）。
    clean_outdir(outdir);
    run_once(&mut bundler, "首次构建");

    // 监听目录：项目根的 `src`（存在则）否则项目根，避开 node_modules/dist。
    let watch_dir = {
        let src = root.join("src");
        if src.is_dir() { src } else { root.clone() }
    };
    let (tx, rx) = mpsc::channel::<()>();
    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res
                && is_source_event(&ev)
            {
                let _ = tx.send(());
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("  {} 无法创建文件监听器：{e}", ui.err("✗"));
                return Err(ExitCode::FAILURE);
            }
        };
    if let Err(e) = watcher.watch(&watch_dir, RecursiveMode::Recursive) {
        eprintln!("  {} 无法监听 {}：{e}", ui.err("✗"), watch_dir.display());
        return Err(ExitCode::FAILURE);
    }
    println!(
        "    {} {} {}",
        ui.dim("监听"),
        ui.dim(&watch_dir.display().to_string()),
        ui.dim("… (Ctrl-C 退出)")
    );
    let _ = std::io::stdout().flush();

    loop {
        // 阻塞等首个事件；断开则退出。
        if rx.recv().is_err() {
            break;
        }
        // 30ms 落盘沉降 + 排空同批事件至 20ms 静默（防抖，对齐 dev server）。
        std::thread::sleep(Duration::from_millis(30));
        while rx.recv_timeout(Duration::from_millis(20)).is_ok() {}
        run_once(&mut bundler, "热重建");
    }
    Ok(())
}

/// notify 事件是否为源码相关（忽略目录/元数据类噪声）。
fn is_source_event(ev: &notify::Event) -> bool {
    use notify::EventKind;
    matches!(
        ev.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) && ev.paths.iter().any(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(is_watched_ext)
    })
}

/// 触发重建的扩展名。
///
/// 除源码外必须包含**图片与字体**：它们既可能被 JS `import`，也可能被 CSS 的 `url()` 引用，
/// 两条路径都会把字节内容（base64 或内容 hash 文件名）打进产物——换一张图不重建就是陈旧产物。
fn is_watched_ext(e: &str) -> bool {
    matches!(
        e,
        "ts" | "tsx"
            | "js"
            | "jsx"
            | "mts"
            | "cts"
            | "json"
            | "css"
            | "raw"
            // 图片
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "svg"
            | "webp"
            | "avif"
            | "ico"
            | "bmp"
            // 字体
            | "woff"
            | "woff2"
            | "ttf"
            | "otf"
            | "eot"
    )
}

/// 构建期 manifest.json：入口文件名 + 各 chunk（文件/类型/模块数/依赖）。供 HTML 注入 / SSR。
fn build_manifest(out: &wake_bundler::BuildOutput) -> String {
    let mut s = String::from("{\n");
    s.push_str(&format!("  \"entry\": {:?},\n", out.entry().file_name));
    s.push_str("  \"chunks\": {\n");
    for (i, c) in out.chunks.iter().enumerate() {
        if i > 0 {
            s.push_str(",\n");
        }
        let imports: Vec<String> = c.imports.iter().map(|f| format!("{f:?}")).collect();
        s.push_str(&format!(
            "    {:?}: {{ \"file\": {:?}, \"kind\": {:?}, \"modules\": {}, \"imports\": [{}] }}",
            c.name,
            c.file_name,
            c.kind.as_str(),
            c.module_ids.len(),
            imports.join(", ")
        ));
    }
    s.push_str("\n  },\n");
    // 带外产物：抽取的 `.css` 与独立资源文件。名字都带内容 hash，SSR / CDN 上传脚本 /
    // 后端模板只能从这里拿到真实文件名——此前 manifest 完全不含这两类，外部消费方拿不到。
    let styles: Vec<String> = out
        .assets
        .iter()
        .filter(|a| a.is_css)
        .map(|a| format!("{:?}", a.file_name))
        .collect();
    let files: Vec<String> = out
        .assets
        .iter()
        .filter(|a| !a.is_css)
        .map(|a| format!("{:?}", a.file_name))
        .collect();
    s.push_str(&format!("  \"styles\": [{}],\n", styles.join(", ")));
    s.push_str(&format!("  \"assets\": [{}],\n", files.join(", ")));
    s.push_str(&format!("  \"modules\": {}\n}}\n", out.module_count));
    s
}

/// 打印品牌横幅：`  ⚡ wake v0.1.0  <sub>`。
fn print_banner(ui: &Ui, sub: &str) {
    println!();
    println!(
        "  {} {} {}  {}",
        ui.warn("⚡"),
        ui.brand("wake"),
        ui.dim(&format!("v{}", env!("CARGO_PKG_VERSION"))),
        ui.dim(sub)
    );
    println!();
}

/// 逐条渲染跨模块诊断（无源码上下文的紧凑形式）。
fn print_diagnostics(ui: &Ui, diags: &[wake_common::Diagnostic]) {
    for d in diags {
        let code = d.code.as_deref().unwrap_or("");
        let (tag, sev) = if d.is_error() {
            (ui.err("error"), d)
        } else {
            (ui.warn("warn"), d)
        };
        eprintln!("  {}{}  {}", tag, ui.dim(&format!("[{code}]")), sev.message);
        // 尾注承载「怎么办」（如不支持的文件类型该改用什么、插值支持哪些表达式）——
        // 此前从未打印，等于用户看不到最有用的那半条信息。
        for note in &d.notes {
            eprintln!("      {} {}", ui.dim("note:"), note);
        }
    }
}

/// `wake dev`：启动 Dev Server + HMR（Phase 5，actix-web）。阻塞直到进程退出。
fn cmd_dev(root: &Path, port: u16) -> Result<(), ExitCode> {
    // 读配置 + 组件扫描 + 别名，交给 dev server 的内部打包器（与 build 一致，@/@@/@@@ 在 dev 也可解析）。
    let (config, _root, aliases) = prepare_project(root);
    let ds = &config.dev_server;

    // https 暂未实现（需 TLS 依赖）——配置了则告警并按 http 起。
    if ds.server.as_deref() == Some("https") {
        eprintln!("warning: dev server 的 https 暂未实现，按 http 启动（后续切片补 TLS）。");
    }
    // 端口：配置 devServer.port 优先，否则用 --port。
    let effective_port = ds.port.unwrap_or(port);
    // 代理规则：config → dev server（pathRewrite BTreeMap → 有序对；ws 暂不支持，仅 HTTP）。
    let proxy: Vec<wake_dev_server::ProxyRule> = ds
        .proxy
        .iter()
        .map(|p| {
            if p.ws {
                eprintln!(
                    "warning: 代理 `{:?}` 的 WebSocket(ws) 暂未实现，仅转发 HTTP。",
                    p.context
                );
            }
            wake_dev_server::ProxyRule {
                context: p.context.clone(),
                target: p.target.clone(),
                path_rewrite: p
                    .path_rewrite
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                change_origin: p.change_origin,
            }
        })
        .collect();

    let options = wake_dev_server::ServeOptions {
        resolve_options: resolve_options(aliases),
        define: build_define(&config, true), // dev 口径：NODE_ENV=development
        host: ds.host.clone().unwrap_or_else(|| "127.0.0.1".to_string()),
        open: ds.open,
        proxy,
    };
    match wake_dev_server::serve(root, effective_port, options) {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("error: dev server 启动失败：{e}");
            Err(ExitCode::FAILURE)
        }
    }
}

fn cmd_parse(file: &Path, ast: bool, style: RenderStyle) -> Result<(), ExitCode> {
    let fs = OsFileSystem;
    let src = match fs.read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: 无法读取 `{}`：{e}", file.display());
            return Err(ExitCode::FAILURE);
        }
    };

    // 源类型：.cjs → 脚本，其余按模块。
    let source_type = if file.extension().is_some_and(|e| e == "cjs") {
        wake_ecma_ast::SourceType::Script
    } else {
        wake_ecma_ast::SourceType::Module
    };

    let interner = wake_common::Interner::new();
    let out = wake_ecma_parser::parse(&src, &interner, source_type);

    // 统计 + 依赖。
    let stmt_count = out.module.with_ast(|p| p.body.len());
    println!(
        "解析 {} —— 顶层语句 {stmt_count} 条，依赖 {} 条",
        file.display(),
        out.dependencies.len()
    );
    for dep in &out.dependencies {
        println!("  {:?}  {}", dep.kind, interner.resolve(dep.specifier));
    }

    // 语义：作用域 / 符号 / 引用（2.5）。
    let model = out.module.with_ast(wake_ecma_parser::analyze);
    println!(
        "作用域 {} 个，符号 {} 个，未解析（全局/未声明）引用 {} 处",
        model.scopes.len(),
        model.symbols.len(),
        model.unresolved_count()
    );

    // --ast：打印 AST 结构。
    if ast {
        out.module.with_ast(|p| {
            println!("\n{p:#?}");
        });
    }

    // 诊断。
    if !out.diagnostics.is_empty() {
        let source = SourceFile::new(file.display().to_string(), src);
        for d in &out.diagnostics {
            eprint!("{}", render(d, &source, style));
        }
        if out.has_errors() {
            return Err(ExitCode::FAILURE);
        }
    }
    Ok(())
}

fn cmd_tokenize(file: &Path, style: RenderStyle) -> Result<(), ExitCode> {
    let fs = OsFileSystem;
    let src = match fs.read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: 无法读取 `{}`：{e}", file.display());
            return Err(ExitCode::FAILURE);
        }
    };

    let (tokens, diags) = wake_ecma_lexer::tokenize(&src);

    for t in &tokens {
        if t.is_eof() {
            continue;
        }
        let text = &src[t.span.lo as usize..t.span.hi as usize];
        let nl = if t.newline_before { " ⏎" } else { "" };
        println!(
            "{:>5}..{:<5} {:<18} {:?}{}",
            t.span.lo,
            t.span.hi,
            t.kind.describe(),
            text,
            nl
        );
    }

    if !diags.is_empty() {
        let source = SourceFile::new(file.display().to_string(), src);
        for d in &diags {
            eprint!("{}", render(d, &source, style));
        }
        return Err(ExitCode::FAILURE);
    }
    Ok(())
}
