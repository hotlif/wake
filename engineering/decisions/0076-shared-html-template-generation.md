# ADR 0076: 开发与生产共享 HTML 模板生成

- Status: accepted
- Date: 2026-11-10

## Context

应用生产构建忽略 html.template，开发服务另行搜索 public/index.html 和 index.html。
项目必须在构建后重写首页，且开发与生产无法保证使用同一外壳。

## Decision

wake_html 拥有唯一模板路径选择、读取错误语义及静态资源注入实现。显式模板相对项目根目录
解析；缺省只读取根目录 index.html，仅该缺省文件不存在时使用内置外壳。
wake_app 解析配置并将模板选择传入开发候选计划；wake_dev_server 不读取项目配置。
模板参与既有候选监听与 HTML/bundle 原子发布，生产 HTML 参与既有目录发布事务。
开发额外注入 Live Reload，Federation 继续使用各自产品的 bootstrap。

## Invariants

- 开发与生产不搜索其他模板位置，显式模板失败不回退。
- 开发挂载根及 index.html 返回同一发布的首页，静态 public 文件不能覆盖该入口。
- 模板读取失败不发布不完整候选；监听覆盖缺失模板及新配置路径以便恢复。
- 服务端只接收已解析模板选择，不拥有第二份配置解释器。

## Evidence

- crates/wake_html/src/lib.rs
- crates/wake_app/src/lib.rs
- crates/wake_dev_server/src/lib.rs
- wake_app 的 html_template_* 回归覆盖共同模板、错误保留与监听恢复；Docs 发布测试覆盖路由外壳。
- wake_html 单元测试覆盖唯一默认路径、显式路径及不可读模板；开发服务回归覆盖 Federation 引导。

## Consequences

旧 public/index.html 模板须显式配置或移到项目根目录。静态 HTML 生成不执行 SSR。
模板选择进入可替换的开发计划；现有候选发布所有权和协议不变。

## Validation

- cargo test -p wake_html -p wake_dev_server -p wake_app --lib
- cargo clippy -p wake_html -p wake_dev_server -p wake_app --all-targets -- -D warnings
- corepack yarn architecture:test
- corepack yarn architecture:check

## Supersedes

None.

## Removal plan

删除开发服务的独立模板搜索及默认外壳实现，不保留隐式 public/index.html 回退。
