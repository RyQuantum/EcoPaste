import {
  type EventCallback,
  type EventName,
  listen,
} from "@tauri-apps/api/event";
import { useEffect, useRef } from "react";
import { log } from "@/utils/log";

/**
 * 订阅 Tauri 事件，组件卸载时自动取消监听。
 * 等价于 `listen(event, handler)` + cleanup，消除样板代码。
 *
 * `listen()` 是异步的：如果组件在它 resolve 之前就卸载了（虚拟列表滚动、刷新时
 * 卡片会频繁挂载/卸载），卸载时还拿不到 unlisten，监听器就会永久泄漏，并且一直
 * 持有那个已卸载组件的旧 handler。剪贴板卡片的 KeyHint（Ctrl+1..0 快速粘贴）
 * 正是通过这里订阅键盘事件，泄漏累积后按一次 Ctrl+N 会同时触发多张旧卡片，
 * 一次粘贴出多条/重复内容。
 *
 * 所以：卸载时打上 disposed 标记；listen() 晚到时若已 disposed，立即 unlisten；
 * 回调里也检查 disposed，保证 unlisten 生效前的窗口期内不会再执行旧 handler。
 * （与 useAppTheme 的 themeMountedRef 处理方式一致。）
 */
export const useTauriListen = <T>(
  event: EventName,
  handler: EventCallback<T>,
) => {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    listen<T>(event, (payload) => {
      if (disposed) return;
      handlerRef.current(payload);
    })
      .then((fn) => {
        if (disposed) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch((error) => {
        log.error(`Failed to listen tauri event: ${event}`, error);
      });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [event]);
};
