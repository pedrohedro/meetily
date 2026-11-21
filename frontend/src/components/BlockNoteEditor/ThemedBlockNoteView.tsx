"use client";

import type { ComponentProps } from "react";
import type { BlockSchema, InlineContentSchema, StyleSchema } from "@blocknote/core";
import { BlockNoteView } from "@blocknote/shadcn";
import { useTheme } from "@/contexts/ThemeContext";

/** BlockNoteView that follows the app theme (light/dark) instead of a fixed one. */
export function ThemedBlockNoteView<B extends BlockSchema, I extends InlineContentSchema, S extends StyleSchema>(
  props: Omit<ComponentProps<typeof BlockNoteView<B, I, S>>, "theme">,
) {
  const { resolved } = useTheme();
  return <BlockNoteView<B, I, S> {...props} theme={resolved} />;
}
