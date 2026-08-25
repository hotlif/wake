# Node、npm 与发布

事实来源：`npm/wake`、根和平台包清单、版本/打包/PnP 脚本、CI 与发布工作流。

- CLI 与 Node 行为通过 `wake_app` 收敛，除非已接受 ADR 改变所有权。
- ESM、CommonJS 和 TypeScript 声明描述同一公共界面；需要同步的 workspace、主包、CSS 包与平台包版本保持一致。
- 破坏性切换原子更新包代码、声明、使用方、测试、文档和发布工作流；打包验证使用全新安装产物。

最低验证：Node 测试和 TypeScript 检查。按风险追加原生构建、启动、打包、全新安装，以及平台/PnP/注册表冒烟检查。
