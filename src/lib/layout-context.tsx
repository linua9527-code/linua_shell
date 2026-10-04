import React, { createContext, useContext, useState, useEffect, useCallback } from 'react';
import { LayoutConfig, LayoutManager } from './layout-config';

interface LayoutContextType {
  layout: LayoutConfig;
  toggleLeftSidebar: () => void;
  toggleBottomPanel: () => void;
  toggleZenMode: () => void;
  setLeftSidebarSize: (size: number) => void;
  setBottomPanelSize: (size: number) => void;
  applyPreset: (presetName: string) => void;
  resetLayout: () => void;
}

const LayoutContext = createContext<LayoutContextType | undefined>(undefined);

export function LayoutProvider({ children }: { children: React.ReactNode }) {
  const [layout, setLayout] = useState<LayoutConfig>(() => LayoutManager.loadLayout());

  // Save layout whenever it changes
  useEffect(() => {
    LayoutManager.saveLayout(layout);
  }, [layout]);

  const toggleLeftSidebar = useCallback(() => {
    setLayout(prev => ({
      ...prev,
      leftSidebarVisible: !prev.leftSidebarVisible,
    }));
  }, []);

  const toggleBottomPanel = useCallback(() => {
    setLayout(prev => ({
      ...prev,
      bottomPanelVisible: !prev.bottomPanelVisible,
    }));
  }, []);

  const toggleZenMode = useCallback(() => {
    setLayout(prev => {
      const newZenMode = !prev.zenMode;
      return {
        ...prev,
        zenMode: newZenMode,
        leftSidebarVisible: !newZenMode && prev.leftSidebarSize > 0,
        bottomPanelVisible: !newZenMode && prev.bottomPanelSize > 0,
      };
    });
  }, []);

  const setLeftSidebarSize = useCallback((size: number) => {
    setLayout(prev => ({ ...prev, leftSidebarSize: size }));
  }, []);

  const setBottomPanelSize = useCallback((size: number) => {
    setLayout(prev => ({ ...prev, bottomPanelSize: size }));
  }, []);

  const applyPreset = useCallback((presetName: string) => {
    const newLayout = LayoutManager.applyPreset(presetName);
    setLayout(newLayout);
  }, []);

  const resetLayout = useCallback(() => {
    const newLayout = LayoutManager.resetLayout();
    setLayout(newLayout);
  }, []);

  return (
    <LayoutContext.Provider
      value={{
        layout,
        toggleLeftSidebar,
        toggleBottomPanel,
        toggleZenMode,
        setLeftSidebarSize,
        setBottomPanelSize,
        applyPreset,
        resetLayout,
      }}
    >
      {children}
    </LayoutContext.Provider>
  );
}

export function useLayout() {
  const context = useContext(LayoutContext);
  if (!context) {
    throw new Error('useLayout must be used within a LayoutProvider');
  }
  return context;
}
