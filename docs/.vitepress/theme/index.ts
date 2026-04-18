import DefaultTheme from "vitepress/theme";
import type { Theme } from "vitepress";
import { h } from "vue";
import BenchChart from "./BenchChart.vue";
import DocsVibeWarning from "./DocsVibeWarning.vue";
import EndevFooter from "./EndevFooter.vue";
import HomeLanding from "./HomeLanding.vue";
import "./custom.css";

export default {
  extends: DefaultTheme,
  Layout() {
    return h(DefaultTheme.Layout, null, {
      "doc-before": () => h(DocsVibeWarning),
      "layout-bottom": () => h(EndevFooter),
    });
  },
  enhanceApp({ app }) {
    app.component("BenchChart", BenchChart);
    app.component("HomeLanding", HomeLanding);
  },
} satisfies Theme;
