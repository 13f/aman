进行 agent 框架的架构设计：

- 业务逻辑见设计文档：/Users/jerin/projects/aman/docs/agent-design.md
- 开发语言：Rust
- 支持两种交互方式
  - CLI
  - 基于 tauri v2 框架实现的桌面app
- 支持当前agent 框架的SOUL 设计
- skills：支持检索查询、热加载、版本控制
- 支持 hooks
- 支持 插件扩展

要求：只做系统设计，不要拆分roadmap/里程碑，这是下一阶段的任务。

设计文档保存到： /Users/jerin/projects/aman/docs/architect-design.md

***

根据业务逻辑设计文档（ /Users/jerin/projects/aman/docs/agent-design.md ）和架构设计文档（ /Users/jerin/projects/aman/docs/architect-design.md ），规划、编写 roadmap：

- 拆分成一系列的里程碑
- 每个里程碑包括具体的待办任务（具体编码的开发者可以直接落实）

保存到： /Users/jerin/projects/aman/docs/milestone.md

***

根据业务逻辑设计文档（ /Users/jerin/projects/aman/docs/agent-design.md ）和架构设计文档（ /Users/jerin/projects/aman/docs/architect-design.md ），评估项目实施的里程碑文档（ /Users/jerin/projects/aman/docs/milestone.md ），是否符合业务逻辑和架构设计。
如果不符合，需要调整里程碑文档，直到符合业务逻辑和架构设计。

***

评估 /Users/jerin/projects/aman/ 目录下的代码，是否已经覆盖 里程碑文档（ /Users/jerin/projects/aman/docs/milestone.md ） 中的M1；并评估代码质量。

