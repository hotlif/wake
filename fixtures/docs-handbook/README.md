# Wake 用户文档可执行示例

这里的源码对应用户教程。`app` 包含首次应用、独立 Counter、逻辑/Mock/时间/网络/React 测试；
`library` 对应工具包构建；`site` 对应最小 Docs 与 Demo；`federation` 是两个本地项目。
app 中的 `tools` 还包含教程中的 Node API 脚本和代理后端；`verify-node.mjs` 验证 API 生命周期。
Docs UI 使用相邻 `fixtures/docs-ui` 的默认、局部与完整覆盖配置。

## 从 Wake 仓库运行

先安装仓库锁定依赖，并构建当前 CLI 与测试 host：

```bash
cargo build -p wake_cli -p wake_test_host
```

从示例 app 目录执行根仓库构建出的 wake（Windows 添加 .exe）：

```bash
../../../target/debug/wake dev .
../../../target/debug/wake build --outdir dist
../../../target/debug/wake test --serial
```

从根目录执行 `corepack yarn exec pnpify tsc -p fixtures/docs-handbook/app/tsconfig.json` 检查类型。
`library` 目录使用 `wake library build . --entry src/index.ts`；`site` 目录使用
`wake docs build . --outdir docs-dist`。这些命令使用源码实现，避免通过旧发布包验证新文档能力。

## Federation

在 remote 目录先启动 `wake dev . --port 4174`，然后在 host 目录执行 `wake federation init .`
并启动 `wake dev . --port 4173`。点击 host 的按钮应显示 `Hello, Wake`。两边都正常关闭后再清理
生成目录。生产 HTTPS 部署需要使用者自己的目标，这里不自动发布或联网生成生产 lock。

## 独立复制

复制某个示例目录后，按 package.json 安装依赖；只有包含所用公开接口的 Wake 版本才适用。
不复制 `.wake`、输出目录或本机安装缓存。应用教程中的命令从 app 根运行，不从外层集合目录运行。

这是操作示例，不改变功能状态；Wake Test 和 library build 仍为 experimental。

本轮实际执行结果和未验证范围见 [VALIDATION.md](VALIDATION.md)。
