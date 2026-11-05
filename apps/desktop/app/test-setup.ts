// Unit tests read the Chinese source text (M7.5): whatever language the machine runs in.
import { setLang } from "@/lib/i18n";
import { applyTheme } from "@/lib/theme";

setLang("zh-CN");
// and paint in the dark theme, the base tokens most of them compare with: without a window there is
// no system theme, which comes to light (lib/theme.ts); a test of the light theme switches to it
applyTheme("dark");
