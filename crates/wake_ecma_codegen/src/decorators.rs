//! TC39 **Stage-3 装饰器**降级（对齐 tsc 的 `--target es2022` emit）。
//!
//! 一个含装饰器的类被包进 IIFE，用两个运行时辅助完成语义：
//! - [`ES_DECORATE`]（`__esDecorate`）：对单个元素应用其装饰器序列（**倒序**应用，
//!   与源码序求值相反），处理 method/getter/setter/field/accessor/class 六种 `kind`；
//! - [`RUN_INITIALIZERS`]（`__runInitializers`）：按序执行收集到的初始化器。
//!
//! ```text
//! @dec class C { @dec m() {} }
//! ↓
//! let C = (() => {
//!   let _instanceExtraInitializers = [];
//!   let _m_decorators, _classDecorators = [dec], _classDescriptor, _classThis,
//!       _classExtraInitializers = [];
//!   var C = class {
//!     static { _classThis = this; }
//!     static {
//!       _m_decorators = [dec];
//!       __esDecorate(this, null, _m_decorators, { kind:"method", name:"m", … }, null,
//!                    _instanceExtraInitializers);
//!       __esDecorate(null, _classDescriptor = { value: _classThis }, _classDecorators,
//!                    { kind:"class", name:_classThis.name }, null, _classExtraInitializers);
//!       C = _classThis = _classDescriptor.value;
//!       __runInitializers(_classThis, _classExtraInitializers);
//!     }
//!     m() {}
//!     constructor() { __runInitializers(this, _instanceExtraInitializers); }
//!   };
//!   return C = _classThis;
//! })();
//! ```
//!
//! plain/experimental AST emitter 尚不降级 `accessor` auto-accessor 字段（需要私有存储与
//! get/set 对），因此只保留原始语法。生产 build/optimize 路径在 owned IR 中完整
//! materialize 装饰器，不经过这里的语法保留分支。

/// `__esDecorate`：对一个类元素应用装饰器序列。语义逐字对齐 tsc 的同名 helper。
pub(crate) const ES_DECORATE: &str = "var __esDecorate=function(ctor,descriptorIn,decorators,contextIn,initializers,extraInitializers){function accept(f){if(f!==void 0&&typeof f!==\"function\")throw new TypeError(\"Function expected\");return f}var kind=contextIn.kind,key=kind===\"getter\"?\"get\":kind===\"setter\"?\"set\":\"value\";var target=!descriptorIn&&ctor?contextIn[\"static\"]?ctor:ctor.prototype:null;var descriptor=descriptorIn||(target?Object.getOwnPropertyDescriptor(target,contextIn.name):{});var _,done=false;for(var i=decorators.length-1;i>=0;i--){var context={};for(var p in contextIn)context[p]=p===\"access\"?{}:contextIn[p];for(var p in contextIn.access)context.access[p]=contextIn.access[p];context.addInitializer=function(f){if(done)throw new TypeError(\"Cannot add initializers after decoration has completed\");extraInitializers.push(accept(f||null))};var result=(0,decorators[i])(kind===\"accessor\"?{get:descriptor.get,set:descriptor.set}:descriptor[key],context);if(kind===\"accessor\"){if(result===void 0)continue;if(result===null||typeof result!==\"object\")throw new TypeError(\"Object expected\");if(_=accept(result.get))descriptor.get=_;if(_=accept(result.set))descriptor.set=_;if(_=accept(result.init))initializers.unshift(_)}else if(_=accept(result)){if(kind===\"field\")initializers.unshift(_);else descriptor[key]=_}}if(target)Object.defineProperty(target,contextIn.name,descriptor);done=true};";

/// `__runInitializers`：按序执行初始化器；带 `value` 时串联传递（字段初始化用）。
pub(crate) const RUN_INITIALIZERS: &str = "var __runInitializers=function(thisArg,initializers,value){var useValue=arguments.length>2;for(var i=0;i<initializers.length;i++){value=useValue?initializers[i].call(thisArg,value):initializers[i].call(thisArg)}return useValue?value:void 0};";

/// 被装饰元素的 `kind`（对应 `__esDecorate` 的 `contextIn.kind`）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecoratedKind {
    Method,
    Getter,
    Setter,
    Field,
    Class,
}

impl DecoratedKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            DecoratedKind::Method => "method",
            DecoratedKind::Getter => "getter",
            DecoratedKind::Setter => "setter",
            DecoratedKind::Field => "field",
            DecoratedKind::Class => "class",
        }
    }
}

/// 规整标识符片段，用于生成 `_<name>_decorators` 这类内部变量名。
pub(crate) fn sanitize(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out.push('_');
    }
    out
}
