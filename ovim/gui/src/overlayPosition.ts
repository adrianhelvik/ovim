export type AnchoredOverlay = {
    left: number;
    top: number;
    width: number;
    height: number;
};

export const anchoredOverlayPosition = (options: {
    anchorX: number;
    anchorY: number;
    containerWidth: number;
    containerHeight: number;
    preferredWidth: number;
    preferredHeight: number;
    padding?: number;
    gap?: number;
}): AnchoredOverlay => {
    const padding = options.padding ?? 8;
    const gap = options.gap ?? 4;
    const width = Math.max(
        1,
        Math.min(options.preferredWidth, options.containerWidth - padding * 2),
    );
    const height = Math.max(
        1,
        Math.min(
            options.preferredHeight,
            options.containerHeight - padding * 2,
        ),
    );
    const maximumLeft = Math.max(
        padding,
        options.containerWidth - width - padding,
    );
    const left = Math.min(Math.max(padding, options.anchorX), maximumLeft);
    const below = options.anchorY + gap;
    const above = options.anchorY - height - gap;
    const top =
        below + height <= options.containerHeight - padding || above < padding
            ? Math.min(
                  Math.max(padding, below),
                  Math.max(padding, options.containerHeight - height - padding),
              )
            : above;

    return { left, top, width, height };
};
