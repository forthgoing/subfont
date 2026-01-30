import { spawnSync } from "node:child_process";
import path from "node:path";
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { createJiti } from "jiti";
import crypto from "node:crypto";
import { createRequire } from "node:module";
import type { Plugin, ResolvedConfig } from "vite";
import type { AstroIntegration } from "astro";

const require = createRequire(import.meta.url);
const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const VIRTUAL_ID_PREFIX = "forthgoing:subfont/";
const VIRTUAL_QUERY = "?forthgoing-virtual";
const VIRTUAL_ID_PREFIX_LENGTH = VIRTUAL_ID_PREFIX.length;

export interface FontConfig {
  preload?: boolean;
  tagPlacement?: "head" | "body";
  stylePlacement?: "head" | "body";
  alias?: string;
  display?: string;
  weight?: string;
}

export type SubfontConfig = Record<string, FontConfig>;

export interface PluginOptions {
  assetsFolder?: string;
  fontsSubdir?: string;
  hashFonts?: boolean;
}

let fontUrlMap: Record<string, string> = {};
let fontPathMap: Record<string, string> = {};

function getBinaryPath(): string {
  const { platform, arch } = process;
  const binName = platform === "win32" ? "subfont-core.exe" : "subfont-core";

  const devPath = path.join(
    __dirname,
    "..",
    "..",
    "core",
    "target",
    "release",
    binName
  );
  if (fs.existsSync(devPath)) return devPath;

  let isMusl = false;
  if (platform === "linux") {
    try {
      const report: any = process.report?.getReport() || {};
      isMusl = !report.header?.glibcVersionRuntime;
    } catch {
      try {
        const { execSync } = require("node:child_process");
        isMusl = execSync("ldd --version", { encoding: "utf8" }).includes(
          "musl"
        );
      } catch {}
    }
  }

  const subPkg = isMusl
    ? `@forthgoing/subfont-linux-musl`
    : `@forthgoing/subfont-${platform}-${arch}`;

  try {
    return require.resolve(`${subPkg}/bin/${binName}`);
  } catch (e) {
    throw new Error(
      `🡭 [Forthgoing:Subfont] Native binary not found for ${subPkg}.\n` +
        `Please ensure you have installed this package correctly.`
    );
  }
}

function runSubsetter(root: string): void {
  const gitignore = path.join(root, ".gitignore");
  if (fs.existsSync(gitignore)) {
    const content = fs.readFileSync(gitignore, "utf8");
    if (!content.includes(".subfont/*")) {
      fs.appendFileSync(
        gitignore,
        "\n# Subfont directory\n" + ".subfont/*\n" + "!.subfont/source\n"
      );
    }
  }

  const binaryPath = getBinaryPath();

  if (!fs.existsSync(binaryPath)) {
    console.error(`🡭 [Forthgoing Subfont] Binary not found: ${binaryPath}`);
    return;
  }

  console.log("🡭 [Forthgoing Subfont] Processing assets...");
  const result = spawnSync(binaryPath, [], {
    stdio: "inherit",
    env: { ...process.env, PROJECT_ROOT: root },
  });

  if (result.status !== 0) {
    console.error("🡭 [Forthgoing Subfont] Subset error occurred.");
  }
}

function populateMaps(root: string, hashFonts: boolean, prefix: string) {
  fontUrlMap = {};
  fontPathMap = {};

  const manifestPath = path.join(root, ".subfont/font-manifest.json");
  if (!fs.existsSync(manifestPath)) {
    console.warn(
      "🡭 [Forthgoing Subfont] Manifest not found - no fonts to process."
    );
    return;
  }

  let manifest: Record<string, string> = {};
  try {
    manifest = JSON.parse(fs.readFileSync(manifestPath, "utf-8"));
  } catch (e) {
    console.error(
      "🡭 [Forthgoing Subfont] Failed to parse font-manifest.json",
      e
    );
    return;
  }

  for (const [key, file] of Object.entries(manifest)) {
    const fullPath = path.join(root, "src/assets/fonts", file);
    if (!fs.existsSync(fullPath)) {
      console.warn(
        `🡭 [Forthgoing Subfont] Font file for "${key}" not found: ${fullPath}`
      );
      continue;
    }

    let hashedName = file;
    if (hashFonts) {
      try {
        const buffer = fs.readFileSync(fullPath);
        const hash = crypto
          .createHash("sha256")
          .update(buffer)
          .digest("hex")
          .slice(0, 10);
        const { name, ext } = path.parse(file);
        hashedName = `${name}-${hash}${ext}`;
      } catch (e) {
        console.error(`🡭 [Forthgoing Subfont] Failed to hash font ${file}`, e);
        continue;
      }
    }

    const url = `${prefix}${hashedName}`;
    fontUrlMap[key] = url;
    fontPathMap[hashedName] = fullPath;
  }
}

function createVirtualComponents(): Plugin {
  return {
    name: "forthgoing-subfont-virtual-components",
    resolveId(source) {
      if (source.startsWith(VIRTUAL_ID_PREFIX)) {
        const relative = source.slice(VIRTUAL_ID_PREFIX_LENGTH);
        if (relative === "head" || relative === "body") {
          return source + ".astro" + VIRTUAL_QUERY;
        }
      }
      return null;
    },
    async load(id) {
      if (
        !id.startsWith(VIRTUAL_ID_PREFIX) ||
        !id.endsWith(".astro" + VIRTUAL_QUERY)
      )
        return null;
      const baseLength = id.length - (".astro" + VIRTUAL_QUERY).length;
      const relative = id.slice(VIRTUAL_ID_PREFIX_LENGTH, baseLength);
      let placement: "head" | "body" | null = null;
      if (relative === "head") placement = "head";
      else if (relative === "body") placement = "body";
      if (!placement) return null;

      const root = process.cwd();
      const manifestPath = path.join(root, ".subfont/font-manifest.json");
      const configPath = path.join(root, "subfont.config.ts");

      let manifest: Record<string, string> = {};
      let config: SubfontConfig = {};
      try {
        if (fs.existsSync(manifestPath)) {
          manifest = JSON.parse(
            fs.readFileSync(manifestPath, "utf-8")
          ) as Record<string, string>;
        }
        if (fs.existsSync(configPath)) {
          const jiti = createJiti(import.meta.url);
          const mod = await jiti.import(configPath);
          config = ((mod as any).default || mod) as SubfontConfig;
        }
      } catch (e) {
        console.error(
          "🡭 [Forthgoing Subfont] Failed to read manifest or config",
          e
        );
        return `---\n---\n`;
      }

      if (
        Object.keys(manifest).length === 0 ||
        Object.keys(fontUrlMap).length === 0
      ) {
        return `---\n---\n`;
      }

      let content = "";
      for (const [key, _file] of Object.entries(manifest)) {
        const fontUrl = fontUrlMap[key];
        if (!fontUrl) {
          console.warn(
            `🡭 [Forthgoing Subfont] No URL mapped for font key "${key}" - skipping`
          );
          continue;
        }

        let fontCfg = config[key];
        if (!fontCfg) {
          const lowerKey = key.toLowerCase();
          const foundKey = Object.keys(config).find((k) => {
            const lowerK = k.toLowerCase();
            return lowerK === lowerKey || lowerKey.startsWith(lowerK);
          });
          if (foundKey) fontCfg = config[foundKey];
        }
        fontCfg = fontCfg ?? {};

        const doPreload = fontCfg.preload ?? true;
        const tagPlacement = fontCfg.tagPlacement ?? "head";
        const stylePlacement = fontCfg.stylePlacement ?? "head";

        if (
          placement === "body" &&
          tagPlacement === "head" &&
          stylePlacement === "head"
        ) {
          continue;
        }

        const alias =
          fontCfg.alias ?? key.charAt(0).toUpperCase() + key.slice(1);
        const display = fontCfg.display ?? "swap";
        const weight = fontCfg.weight ?? "100 900";

        if (doPreload && tagPlacement === placement) {
          content += `<link rel="preload" href="${fontUrl}" as="font" type="font/woff2" crossorigin>\n`;
        }
        if (stylePlacement === placement) {
          content += `<style is:global>
@font-face {
  font-family: "${alias}";
  src: url("${fontUrl}") format("woff2");
  font-display: ${display};
  font-weight: ${weight};
}
</style>\n`;
        }
      }

      return `---
---
${content}
`;
    },
  };
}

function createFontServingPlugin(options: PluginOptions): Plugin {
  const assetsFolder = options.assetsFolder || "_astro";
  const fontsSubdir = options.fontsSubdir || "fonts";
  const prefix = `/${assetsFolder}/${fontsSubdir}/`;

  let config: ResolvedConfig;

  return {
    name: "forthgoing-font-serving",
    configResolved(resolvedConfig) {
      config = resolvedConfig;
    },
    configureServer(server) {
      const root = server.config.root ?? process.cwd();

      server.middlewares.use(prefix, (req, res, next) => {
        if (req.method !== "GET" || !req.url) return next();

        let filename = decodeURIComponent(req.url.slice(1));
        const filePath =
          fontPathMap[filename] ||
          path.join(root, "src/assets/fonts", filename);

        if (!fs.existsSync(filePath)) return next();

        try {
          res.setHeader("Content-Type", "font/woff2");
          res.setHeader(
            "Cache-Control",
            "no-store, no-cache, must-revalidate, max-age=0"
          );
          fs.createReadStream(filePath)
            .on("error", (err) => {
              console.error(
                `🡭 [Forthgoing Subfont] Stream error for ${filename}`,
                err
              );
              if (!res.headersSent) next();
            })
            .pipe(res);
        } catch (e) {
          console.error(
            `🡭 [Forthgoing Subfont] Error serving font ${filename}`,
            e
          );
          next();
        }
      });
    },
    buildEnd() {
      if (config.build.ssr) return;
      const root = config.root ?? process.cwd();
      const outDir = path.resolve(root, config.build.outDir ?? "dist");
      const fontsDir = path.join(outDir, assetsFolder, fontsSubdir);
      fs.mkdirSync(fontsDir, { recursive: true });

      let copied = 0;
      for (const [hashedName, srcPath] of Object.entries(fontPathMap)) {
        const destPath = path.join(fontsDir, hashedName);
        try {
          if (fs.existsSync(srcPath)) {
            fs.copyFileSync(srcPath, destPath);
            copied++;
          } else {
            console.warn(
              `🡭 [Forthgoing Subfont] Source missing during copy: ${srcPath}`
            );
          }
        } catch (e) {
          console.error(
            `🡭 [Forthgoing Subfont] Failed to copy ${srcPath} → ${destPath}`,
            e
          );
        }
      }

      if (copied > 0) {
        console.log(
          `🡭 [Forthgoing Subfont] Copied ${copied} font file${
            copied === 1 ? "" : "s"
          } to ${assetsFolder}/${fontsSubdir}/`
        );
      }
    },
  };
}

export default function subFont(options: PluginOptions = {}): AstroIntegration {
  return {
    name: "@forthgoing/subfont",
    hooks: {
      "astro:config:setup": ({ updateConfig, config, command }) => {
        if (command === "preview") return;
        const root = fileURLToPath(config.root);
        const assetsFolder = options.assetsFolder || "_astro";
        const fontsSubdir = options.fontsSubdir || "fonts";
        const prefix = `/${assetsFolder}/${fontsSubdir}/`;
        const hashFonts = options.hashFonts ?? true;

        runSubsetter(root);
        populateMaps(root, hashFonts, prefix);

        updateConfig({
          vite: {
            //@ts-ignore
            plugins: [
              //@ts-ignore
              createFontServingPlugin(options),
              //@ts-ignore
              createVirtualComponents(),
            ],
          },
        });
      },
    },
  };
}
