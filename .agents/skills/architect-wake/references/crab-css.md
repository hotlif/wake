# Crab CSS

事实来源：`engineering/CRAB_CSS.md`、`npm/css`、`wake_css_in_js` 和打包器 CSS 集成测试。

- 静态求值不执行任意用户代码，也不静默丢弃无法解析的样式；动态值通过明确的 CSS 自定义属性边界传递。
- 样式标识不受无关插入影响，并在受支持的路径和平台上稳定。
- CSS 顺序、全局效果、关键帧、URL、chunk、缓存和 HMR 作为一条产物数据流验证。

最低验证：包运行时/类型测试和编译器测试。按风险追加打包器 CSS 测试、pack smoke 与 fixture 构建。
