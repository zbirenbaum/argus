import { cn } from "@utils/cn";
import { createSignal, type JSX } from "solid-js";
import { Show } from "solid-js/web";

interface CollapsibleProps {
  title: JSX.Element;
  defaultOpen?: boolean;
  children: JSX.Element;
}

export function Collapsible(props: CollapsibleProps) {
  const [open, setOpen] = createSignal(props.defaultOpen ?? false);

  return (
    <div>
      <button
        type="button"
        class={cn(
          "flex w-full items-center gap-1.5 px-2 py-1.5 text-sm font-medium",
          "hover:bg-[hsl(var(--muted))] transition-colors rounded-[var(--radius-sm)]",
        )}
        onClick={() => setOpen((prev) => !prev)}
      >
        <svg
          aria-hidden="true"
          class={cn("h-3.5 w-3.5 shrink-0 transition-transform", open() && "rotate-90")}
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
        >
          <path d="M9 18l6-6-6-6" />
        </svg>
        {props.title}
      </button>
      <Show when={open()}>
        <div class="pl-3">{props.children}</div>
      </Show>
    </div>
  );
}
