import { useEventListener } from "ahooks";
import { Input, type InputProps, type InputRef } from "antd";
import type { ChangeEvent, CompositionEvent, FC } from "react";
import { useCallback, useEffect, useRef } from "react";
import { replaySearchKey } from "@/commands";
import KeyHint from "@/components/KeyHint";
import { TAURI_EVENT } from "@/constants/events";
import { prepareClipboardWindowEditableFocus } from "@/hooks/useClipboardWindowEditableFocus";
import { useTauriListen } from "@/hooks/useTauriListen";
import type { SearchHandoffKey } from "@/types/rySettings";
import { isWinClipboardWindow } from "@/utils/is";
import { log } from "@/utils/log";

/** 等窗口真正拿到焦点最多等几帧；拿不到就不重发，免得按键落到背后的程序里。 */
const HANDOFF_FOCUS_MAX_FRAMES = 10;
/** Windows 虚拟键码。 */
const VK_LEFT = 0x25;
const VK_RIGHT = 0x27;

interface SearchInputProps extends Omit<InputProps, "prefix"> {
  blurToken?: number;
  clearToken?: number;
  focusToken?: number;
}

/**
 * 带快捷键提示的搜索输入框，支持 ⌘F / Ctrl+F 聚焦。
 * IME 拼音/日文组合输入期间抑制 onChange，待 compositionend 再补发一次，
 * 避免上层防抖/受控逻辑被中间态拼字串污染。
 */
const SearchInput: FC<SearchInputProps> = (props) => {
  const {
    blurToken = 0,
    clearToken = 0,
    focusToken = 0,
    onChange,
    onCompositionStart,
    onCompositionEnd,
    ...rest
  } = props;

  const inputRef = useRef<InputRef>(null);
  const composingRef = useRef(false);

  /**
   * 聚焦搜索框并选中已有内容，便于直接覆盖输入。
   */
  const focusSearch = useCallback(async () => {
    if (!inputRef.current) return;

    await prepareClipboardWindowEditableFocus();
    inputRef.current?.focus({ cursor: "all" });
  }, []);

  /**
   * fork：焦点在列表上时，导航钩子吞掉了本该给搜索框的键（←/→、「只用 Alt」档下的 Ctrl 编辑组合）。
   * 把焦点交回搜索框，保留原来的光标和选区，再让 Rust 把这个键重发一次，由输入框原生处理。
   */
  const handOffToSearch = async (key: SearchHandoffKey) => {
    const input = inputRef.current?.input;
    if (!input) return;

    await prepareClipboardWindowEditableFocus();
    input.focus();

    for (let frame = 0; frame < HANDOFF_FOCUS_MAX_FRAMES; frame++) {
      await nextFrame();
      if (document.hasFocus() && document.activeElement === input) {
        await replaySearchKey(key);
        return;
      }
    }

    log.warn("search handoff: clipboard window did not get focus, key dropped");
  };

  const handleSearchHandoff = (event: { payload: SearchHandoffKey }) => {
    if (!isWinClipboardWindow()) return;

    void handOffToSearch(event.payload);
  };

  useTauriListen(TAURI_EVENT.KEYBOARD_SEARCH_HANDOFF, handleSearchHandoff);

  /**
   * fork：搜索框刚把焦点交还列表、窗口还没变回不可聚焦的那几十毫秒里，←/→ 进的是网页本身，
   * 导航钩子收不到。焦点停在 body 上时同样交回搜索框重发，Ctrl 档下快速按 Ctrl+← 也不会丢键。
   */
  const handleWindowKeyDown = (event: KeyboardEvent) => {
    if (!isWinClipboardWindow()) return;
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    if (document.activeElement && document.activeElement !== document.body)
      return;

    event.preventDefault();
    void handOffToSearch({
      ctrl: event.ctrlKey,
      shift: event.shiftKey,
      vk: event.key === "ArrowLeft" ? VK_LEFT : VK_RIGHT,
    });
  };

  useEventListener("keydown", handleWindowKeyDown, { target: window });

  useEffect(() => {
    if (blurToken <= 0) return;

    inputRef.current?.blur();
  }, [blurToken]);

  useEffect(() => {
    if (focusToken <= 0) return;

    const frame = requestAnimationFrame(() => {
      void focusSearch();
    });

    return () => {
      cancelAnimationFrame(frame);
    };
  }, [focusToken, focusSearch]);

  const handleChange = (event: ChangeEvent<HTMLInputElement>) => {
    if (composingRef.current) return;

    onChange?.(event);
  };

  const handleCompositionStart = (
    event: CompositionEvent<HTMLInputElement>,
  ) => {
    composingRef.current = true;

    onCompositionStart?.(event);
  };

  const handleCompositionEnd = (event: CompositionEvent<HTMLInputElement>) => {
    composingRef.current = false;

    onCompositionEnd?.(event);
    // composition 结束时浏览器已派发最后一次 input，但被上面挡掉了，这里补一次。
    onChange?.(event as unknown as ChangeEvent<HTMLInputElement>);
  };

  return (
    <Input
      autoCapitalize="off"
      autoCorrect="off"
      data-allow-global-keyboard="true"
      key={clearToken}
      onChange={handleChange}
      onCompositionEnd={handleCompositionEnd}
      onCompositionStart={handleCompositionStart}
      prefix={
        <KeyHint
          hintKey="F"
          iconName="i-lucide:search"
          onKeyPress={focusSearch}
        />
      }
      ref={inputRef}
      spellCheck={false}
      {...rest}
    />
  );
};

export default SearchInput;

function nextFrame() {
  return new Promise<void>((resolve) => {
    requestAnimationFrame(() => {
      resolve();
    });
  });
}
