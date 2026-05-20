
prompt engineering 就是给模型写清楚指令、提供完成任务所需的上下文，并思考如何组织这些信息。使用前，先准备好三样东西：
* 成功标准的明确定义
* 可经验测试的评估方法
* 一个初稿 prompt

即 prompt 工程的最佳实践要能覆盖清晰度、示例、XML 结构、思考链、agentic systems、tool use、输出控制等完整技术栈。

## 第一层：先告诉模型"你来干嘛的"

任务描述和角色定义放在 prompt 最前面。这一步把模型从通用聊天模式拉进具体业务场景。

## 第二层：喂上下文，区分动态内容和固定背景

动态内容是每次请求不同的参数、需求等等；

固定背景是已经确定的模板、程序等。这类固定背景特别适合放进 system prompt，也适合做 prompt caching（缓存），因为它每次调用都一样。

## 第三层：步骤顺序很关键

注意：order matters

给模型的处理顺序，本质上就是你把人类专家的工作流程写给了它。


## 第四层：用 XML tags 做结构化分隔

结构和组织，XML tags 等分隔符能帮 LLM 理解每块信息的边界，能告诉模型"这段是什么"，方便后续引用和解析。

## 第五层：用 examples 引导模型判断

examples/few-shot 是引导 LLM 大模型 的强力机制。尤其是那些模型容易判断错的灰色案例，放进 system prompt 当参照，下次遇到相似情况就有锚点。

## 第六层：防幻觉——该说"不确定"就说"不确定"

防止幻觉——不要编造 prompt 里没有的细节。
建议在 prompt 尾部加一个 reminder：如果条件、参数有所遗漏、缺失，或者给定/上传的图片、表格看不清楚，承认无法确定，而不要猜。


## 第七层：输出格式要对接下游系统

把最终结论包在 XML tags 里，或者用 prefill/JSON 格式输出——目的是让结果可以直接进数据库、进分析系统、进下一个流程（比如评估管道等）。

## 其它

更多内容可以参考 Claude 的官方文档和 [prompt-eng-interactive-tutorial](https://github.com/anthropics/prompt-eng-interactive-tutorial)。
