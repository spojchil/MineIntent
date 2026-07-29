# agent-service

本目录包含当前 Python 模型进程。项目级说明统一维护在：

- [运行指南](../docs/guides/run.md)：配置、启动、健康检查和敏感数据；
- [当前实现结构](../docs/architecture.md)：Node/Python 边界、工具循环和取消语义；
- [验证指南](../docs/guides/validation.md)：Python 测试及其证据边界。

目录内接口和参数的最终产生方是 [`server.py`](./server.py) 与对应测试。
