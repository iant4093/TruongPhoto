import React, { useEffect, useRef } from 'react';
import Hls from 'hls.js';
import type { PlaybackInfo } from '../types/api';

export interface VideoPlayerProps {
  playback: PlaybackInfo;
  posterUrl?: string;
  autoPlay?: boolean;
  initialTimestampSec?: number | null;
  className?: string;
}

export const VideoPlayer: React.FC<VideoPlayerProps> = ({
  playback,
  posterUrl,
  autoPlay = false,
  initialTimestampSec = null,
  className,
}) => {
  const videoRef = useRef<HTMLVideoElement | null>(null);

  const targetTimestampSec =
    typeof initialTimestampSec === 'number' &&
    Number.isFinite(initialTimestampSec) &&
    initialTimestampSec > 0
      ? initialTimestampSec
      : null;

  useEffect(() => {
    const video = videoRef.current;
    if (!video) {
      return;
    }

    let didSeek = false;
    const seekToInitial = () => {
      if (didSeek || targetTimestampSec === null) return;
      didSeek = true;
      try {
        video.currentTime = targetTimestampSec;
      } catch {
        // Ignore seek errors before media is seekable
      }
    };

    const onLoadedMetadata = () => {
      seekToInitial();
    };

    if (playback.mode !== 'hls' || !playback.hls_url) {
      if (targetTimestampSec !== null) {
        if (video.readyState >= 1) {
          seekToInitial();
        } else {
          video.addEventListener('loadedmetadata', onLoadedMetadata, { once: true });
        }
      }
      return () => {
        video.removeEventListener('loadedmetadata', onLoadedMetadata);
      };
    }

    const hlsUrl = playback.hls_url;
    const canPlayNativeHls = video.canPlayType('application/vnd.apple.mpegurl') !== '';

    if (canPlayNativeHls) {
      if (targetTimestampSec !== null) {
        video.addEventListener('loadedmetadata', onLoadedMetadata, { once: true });
      }
      video.src = hlsUrl;
      if (video.readyState >= 1) {
        seekToInitial();
      }
      if (autoPlay) {
        void video.play().catch(() => {});
      }
      return () => {
        video.removeEventListener('loadedmetadata', onLoadedMetadata);
        video.pause();
        video.removeAttribute('src');
        video.load();
      };
    }

    if (Hls.isSupported()) {
      const hls = new Hls({
        xhrSetup: (xhr) => {
          xhr.withCredentials = true;
        },
      });
      if (targetTimestampSec !== null) {
        video.addEventListener('loadedmetadata', onLoadedMetadata, { once: true });
      }
      hls.loadSource(hlsUrl);
      hls.attachMedia(video);
      hls.on(Hls.Events.MANIFEST_PARSED, () => {
        if (targetTimestampSec !== null && video.readyState >= 1) {
          seekToInitial();
        } else if (targetTimestampSec !== null) {
          try {
            video.currentTime = targetTimestampSec;
          } catch {
            // Will seek on loadedmetadata once init segment parses
          }
        }
        if (autoPlay) {
          void video.play().catch(() => {});
        }
      });
      return () => {
        video.removeEventListener('loadedmetadata', onLoadedMetadata);
        hls.destroy();
      };
    }

    if (playback.direct_url) {
      if (targetTimestampSec !== null) {
        video.addEventListener('loadedmetadata', onLoadedMetadata, { once: true });
      }
      video.src = playback.direct_url;
      if (video.readyState >= 1) {
        seekToInitial();
      }
      return () => {
        video.removeEventListener('loadedmetadata', onLoadedMetadata);
        video.pause();
        video.removeAttribute('src');
        video.load();
      };
    }

    return undefined;
  }, [playback.mode, playback.hls_url, playback.direct_url, autoPlay, targetTimestampSec]);

  const directSrc =
    playback.mode === 'direct' ? (playback.direct_url ?? playback.original_url) : undefined;

  return (
    <video
      ref={videoRef}
      src={directSrc}
      poster={posterUrl}
      controls
      playsInline
      autoPlay={autoPlay}
      className={className}
    />
  );
};
