import { useEffect, useRef, useState } from "react";
import { toast, Toaster } from "sonner";
import { sendBugReport } from "@/lib/bugReport";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { platform } from "@tauri-apps/plugin-os";
import {
  checkAccessibilityPermission,
  checkMicrophonePermission,
} from "tauri-plugin-macos-permissions-api";
import { ModelStateEvent, RecordingErrorEvent } from "./lib/types/events";
import "./App.css";
import AccessibilityPermissions from "./components/AccessibilityPermissions";
import Footer from "./components/footer";
import Onboarding, { AccessibilityOnboarding } from "./components/onboarding";
import { Sidebar, SidebarSection, SECTIONS_CONFIG } from "./components/Sidebar";
import { useSettings } from "./hooks/useSettings";
import { useSettingsStore } from "./stores/settingsStore";
import { commands } from "@/bindings";
import { getLanguageDirection, initializeRTL } from "@/lib/utils/rtl";

type OnboardingStep = "accessibility" | "model" | "done";

const renderSettingsContent = (section: SidebarSection) => {
  const ActiveComponent =
    SECTIONS_CONFIG[section]?.component || SECTIONS_CONFIG.general.component;
  return <ActiveComponent />;
};

const TWEAK_DEFAULTS = /*EDITMODE-BEGIN*/ {
  accentColor: "#8fb4d9",
  paperTint: "#fcf4dc",
  density: 1,
  radiusScale: 1,
  sketchOpacity: 0.14,
}; /*EDITMODE-END*/

void TWEAK_DEFAULTS;

function App() {
  const { t, i18n } = useTranslation();
  const [onboardingStep, setOnboardingStep] = useState<OnboardingStep | null>(
    null,
  );
  const [isReturningUser, setIsReturningUser] = useState(false);
  const [currentSection, setCurrentSection] =
    useState<SidebarSection>("listening");
  const { settings, updateSetting } = useSettings();
  const direction = getLanguageDirection(i18n.language);
  const refreshAudioDevices = useSettingsStore(
    (state) => state.refreshAudioDevices,
  );
  const refreshOutputDevices = useSettingsStore(
    (state) => state.refreshOutputDevices,
  );
  const hasCompletedPostOnboardingInit = useRef(false);
  const activeSectionConfig =
    SECTIONS_CONFIG[currentSection] ?? SECTIONS_CONFIG.general;
  const activeSectionLabel = t(activeSectionConfig.labelKey, {
    defaultValue: activeSectionConfig.defaultLabel,
  });
  const sectionDescriptions: Record<SidebarSection, string> = {
    listening: t("app.sectionIntro.listening", {
      defaultValue:
        "Règle la balance entre vitesse et précision, et active les modes spéciaux quand t'en as besoin.",
    }),
    words: t("app.sectionIntro.words", {
      defaultValue:
        "Ajoute les noms, expressions et corrections que tu veux que MotsDits reconnaisse sans hésiter.",
    }),
    history: t("app.sectionIntro.history", {
      defaultValue:
        "Tes dernières dictées, gardées localement. Réécoute, copie, supprime -- toi, tu décides.",
    }),
    general: t("app.sectionIntro.general", {
      defaultValue:
        "Les réglages du quotidien : raccourci, microphone, lancement au démarrage.",
    }),
    postprocessing: t("app.sectionIntro.postprocessing", {
      defaultValue:
        "Une touche d'IA pour polir ta dictée selon ton domaine -- juridique, comptable, médical et plus.",
    }),
    advanced: t("app.sectionIntro.advanced", {
      defaultValue:
        "Les options plus fines, pour ceux qui veulent ajuster le détail. Les valeurs par défaut conviennent à la plupart.",
    }),
    debug: t("app.sectionIntro.debug", {
      defaultValue:
        "Les outils de diagnostic, à utiliser quand quelque chose cloche.",
    }),
    about: t("app.sectionIntro.about", {
      defaultValue:
        "Version, langue, et les crédits des composants ouverts qui font tourner MotsDits.",
    }),
  };
  const activeSectionDescription = sectionDescriptions[currentSection];
  const sectionKickers: Record<SidebarSection, string> = {
    listening: t("app.sectionIntro.kicker.listening", {
      defaultValue: "Ton écoute",
    }),
    words: t("app.sectionIntro.kicker.words", {
      defaultValue: "Ton vocabulaire",
    }),
    history: t("app.sectionIntro.kicker.history", {
      defaultValue: "Ta mémoire",
    }),
    general: t("app.sectionIntro.kicker.general", {
      defaultValue: "Tes bases",
    }),
    postprocessing: t("app.sectionIntro.kicker.postprocessing", {
      defaultValue: "Ton polissage",
    }),
    advanced: t("app.sectionIntro.kicker.advanced", {
      defaultValue: "Plus loin",
    }),
    debug: t("app.sectionIntro.kicker.debug", {
      defaultValue: "Diagnostic",
    }),
    about: t("app.sectionIntro.kicker.about", {
      defaultValue: "À propos",
    }),
  };
  const activeSectionKicker = sectionKickers[currentSection];

  useEffect(() => {
    checkOnboardingStatus();
  }, []);

  useEffect(() => {
    initializeRTL(i18n.language);
  }, [i18n.language]);

  useEffect(() => {
    if (onboardingStep === "done" && !hasCompletedPostOnboardingInit.current) {
      hasCompletedPostOnboardingInit.current = true;
      const initializeAppState = async () => {
        try {
          await commands.initializeShortcuts();
        } catch (e) {
          console.warn("Failed to initialize shortcuts:", e);
        }

        try {
          await commands.initializeEnigo();
        } catch (e) {
          console.warn("Failed to initialize input system:", e);
        }
      };

      initializeAppState();
      refreshAudioDevices();
      refreshOutputDevices();
    }
  }, [onboardingStep, refreshAudioDevices, refreshOutputDevices]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      const isDebugShortcut =
        event.shiftKey &&
        event.key.toLowerCase() === "d" &&
        (event.ctrlKey || event.metaKey);

      if (isDebugShortcut) {
        event.preventDefault();
        const currentDebugMode = settings?.debug_mode ?? false;
        updateSetting("debug_mode", !currentDebugMode);
      }
    };

    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [settings?.debug_mode, updateSetting]);

  useEffect(() => {
    const unlisten = listen<RecordingErrorEvent>("recording-error", (event) => {
      const { error_type, detail } = event.payload;

      if (error_type === "microphone_permission_denied") {
        const currentPlatform = platform();
        const platformKey = `errors.micPermissionDenied.${currentPlatform}`;
        const description = t(platformKey, {
          defaultValue: t("errors.micPermissionDenied.generic"),
        });
        toast.error(t("errors.micPermissionDeniedTitle"), { description });
      } else if (error_type === "microphone_silent") {
        toast.error(t("errors.microphoneSilentTitle"), {
          description: t("errors.microphoneSilent"),
        });
      } else {
        const errMsg = detail ?? "Unknown error";
        toast.error(t("errors.recordingFailed", { error: errMsg }), {
          action: {
            label: "Report",
            onClick: () => void sendBugReport(`Recording failed: ${errMsg}`),
          },
        });
      }
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  useEffect(() => {
    const unlisten = listen<ModelStateEvent>("model-state-changed", (event) => {
      if (event.payload.event_type === "loading_failed") {
        const modelName =
          event.payload.model_name || t("errors.modelLoadFailedUnknown");
        toast.error(t("errors.modelLoadFailed", { model: modelName }), {
          description: event.payload.error,
          action: {
            label: "Report",
            onClick: () =>
              void sendBugReport(
                `Model load failed: ${modelName} -- ${event.payload.error ?? ""}`,
              ),
          },
        });
      }
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  const revealMainWindowForPermissions = async () => {
    try {
      await commands.showMainWindowCommand();
    } catch (e) {
      console.warn("Failed to show main window for permission onboarding:", e);
    }
  };

  const checkOnboardingStatus = async () => {
    setIsReturningUser(true);
    try {
      const currentPlatform = platform();

      if (currentPlatform === "macos") {
        try {
          const [hasAccessibility, hasMicrophone] = await Promise.all([
            checkAccessibilityPermission(),
            checkMicrophonePermission(),
          ]);
          if (!hasAccessibility || !hasMicrophone) {
            await revealMainWindowForPermissions();
            setOnboardingStep("accessibility");
            return;
          }
        } catch (e) {
          console.warn("Failed to check macOS permissions:", e);
        }
      }

      if (currentPlatform === "windows") {
        try {
          const microphoneStatus =
            await commands.getWindowsMicrophonePermissionStatus();
          if (
            microphoneStatus.supported &&
            microphoneStatus.overall_access === "denied"
          ) {
            await revealMainWindowForPermissions();
            setOnboardingStep("accessibility");
            return;
          }
        } catch (e) {
          console.warn("Failed to check Windows microphone permissions:", e);
        }
      }

      setOnboardingStep("done");
    } catch (error) {
      console.error("Failed to check onboarding status:", error);
      setOnboardingStep("done");
    }
  };

  const handleAccessibilityComplete = () => {
    setOnboardingStep("done");
  };

  const handleModelSelected = () => {
    setOnboardingStep("done");
  };

  if (onboardingStep === null) {
    return null;
  }

  if (onboardingStep === "accessibility") {
    return <AccessibilityOnboarding onComplete={handleAccessibilityComplete} />;
  }

  if (onboardingStep === "model") {
    return <Onboarding onModelSelected={handleModelSelected} />;
  }

  return (
    <div
      dir={direction}
      className="paper-app app-shell select-none cursor-default"
    >
      <Toaster
        theme="system"
        toastOptions={{
          unstyled: true,
          classNames: {
            toast:
              "paper-toast px-4 py-3 flex items-center gap-3 text-sm text-text",
            title: "font-medium text-text",
            description: "text-mid-gray",
          },
        }}
      />
      <div className="app-frame">
        <Sidebar
          activeSection={currentSection}
          onSectionChange={setCurrentSection}
        />
        <div className="app-main">
          <main className="app-main-scroll paper-scroll-area">
            <div className="app-content-shell">
              <section className="app-section-intro">
                <p className="app-section-kicker">{activeSectionKicker}</p>
                <h1>{activeSectionLabel}</h1>
                <p>{activeSectionDescription}</p>
              </section>
              <div className="app-content-stack paper-stack">
                <AccessibilityPermissions />
                {renderSettingsContent(currentSection)}
              </div>
            </div>
          </main>
        </div>
      </div>
      <Footer />
    </div>
  );
}

export default App;
