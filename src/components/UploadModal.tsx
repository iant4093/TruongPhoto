import React, { useCallback, useEffect, useRef, useState } from 'react';
import { UploadCloud, X } from 'lucide-react';
import { uploadMediaFiles } from '../lib/api';
import { isTopEscapeLayer, popEscapeLayer, pushEscapeLayer } from '../lib/escapeStack';
import { notifyError, notifySuccess } from '../store/feedback';
import { errorMessage, useAppStore } from '../store/useAppStore';
import type { UploadResponse } from '../types/api';
import { Badge } from './ui';

export interface UploadModalProps {
  open: boolean;
  initialFiles?: File[];
  onClose: () => void;
}

export const UploadModal: React.FC<UploadModalProps> = ({
  open,
  initialFiles = [],
  onClose,
}) => {
  const bumpLibraryRevision = useAppStore((s) => s.bumpLibraryRevision);
  const refreshFolders = useAppStore((s) => s.refreshFolders);
  const refreshSystemInfo = useAppStore((s) => s.refreshSystemInfo);

  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const uploadingRef = useRef(false);
  const [dragOver, setDragOver] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [progress, setProgress] = useState(0);
  const [selectedCount, setSelectedCount] = useState(0);
  const [result, setResult] = useState<UploadResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  const runUpload = useCallback(
    async (files: File[]) => {
      if (files.length === 0 || uploadingRef.current) return;
      uploadingRef.current = true;
      setUploading(true);
      setProgress(0);
      setSelectedCount(files.length);
      setResult(null);
      setError(null);

      try {
        const res = await uploadMediaFiles(files, {
          onProgress: (pct) => setProgress(pct),
        });
        setResult(res);
        setProgress(100);
        if (res.uploaded.length > 0) {
          bumpLibraryRevision();
          void refreshFolders().catch(() => {});
          void refreshSystemInfo().catch(() => {});
          notifySuccess(
            `Uploaded ${res.uploaded.length} item${res.uploaded.length === 1 ? '' : 's'}`,
          );
        }
        if (res.errors.length > 0 && res.uploaded.length === 0) {
          notifyError(new Error(`${res.errors.length} file(s) failed to upload`));
        }
      } catch (err) {
        const msg = errorMessage(err);
        setError(msg);
        notifyError(err, 'Upload failed');
      } finally {
        uploadingRef.current = false;
        setUploading(false);
      }
    },
    [bumpLibraryRevision, refreshFolders, refreshSystemInfo],
  );

  useEffect(() => {
    if (!open) {
      setDragOver(false);
      setProgress(0);
      setSelectedCount(0);
      setResult(null);
      setError(null);
      return;
    }

    const layer = pushEscapeLayer();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && isTopEscapeLayer(layer) && !uploading) {
        event.preventDefault();
        event.stopPropagation();
        onClose();
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
      popEscapeLayer(layer);
    };
  }, [open, uploading, onClose]);

  useEffect(() => {
    if (open && initialFiles.length > 0) {
      void runUpload(initialFiles);
    }
  }, [open, initialFiles, runUpload]);

  if (!open) return null;

  return (
    <div
      className="modal-backdrop"
      onClick={() => {
        if (!uploading) onClose();
      }}
    >
      <div
        className="panel upload-modal stack"
        role="dialog"
        aria-modal="true"
        aria-label="Upload media"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="row" style={{ justifyContent: 'space-between' }}>
          <strong style={{ fontSize: 15 }}>Upload Media</strong>
          <button
            type="button"
            className="icon-button"
            aria-label="Close upload modal"
            disabled={uploading}
            onClick={onClose}
          >
            <X size={16} />
          </button>
        </div>

        <div
          role="button"
          tabIndex={0}
          className={`upload-dropzone${dragOver ? ' is-drag-over' : ''}`}
          aria-label="Select or drop files to upload"
          onClick={() => {
            if (!uploading) fileInputRef.current?.click();
          }}
          onKeyDown={(e) => {
            if ((e.key === 'Enter' || e.key === ' ') && !uploading) {
              e.preventDefault();
              fileInputRef.current?.click();
            }
          }}
          onDragEnter={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setDragOver(true);
          }}
          onDragOver={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setDragOver(true);
          }}
          onDragLeave={(e) => {
            e.stopPropagation();
            setDragOver(false);
          }}
          onDrop={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setDragOver(false);
            if (uploadingRef.current) return;
            const dropped = Array.from(e.dataTransfer.files);
            void runUpload(dropped);
          }}
        >
          <UploadCloud size={28} strokeWidth={1.4} />
          <div style={{ fontWeight: 500, color: 'var(--text-primary)' }}>
            Drop photos or videos here, or click to browse
          </div>
          <div className="view-subtitle" style={{ marginTop: 0 }}>
            Streams directly to Managed Uploads with automatic stacking and thumbnails.
          </div>
          <input
            ref={fileInputRef}
            type="file"
            multiple
            accept="image/*,video/*,.cr3,.cr2,.nef,.arw,.dng,.raf,.orf,.rw2,.heic,.heif,.jxl,.livp"
            style={{ display: 'none' }}
            onChange={(e) => {
              const files = Array.from(e.target.files ?? []);
              e.target.value = '';
              void runUpload(files);
            }}
          />
        </div>

        <div className="view-subtitle" style={{ marginTop: 0 }}>
          Note: Mobile Safari uploads the still frame for Live Photos; drop both still and `.mov`
          files together on desktop for full Live Photo stacks.
        </div>

        {(uploading || progress > 0) && (
          <div className="stack" style={{ gap: 6 }}>
            <div className="row" style={{ justifyContent: 'space-between', fontSize: 12 }}>
              <span>
                {uploading
                  ? `Uploading ${selectedCount} file${selectedCount === 1 ? '' : 's'}…`
                  : 'Upload finished'}
              </span>
              <span style={{ fontVariantNumeric: 'tabular-nums' }}>{progress}%</span>
            </div>
            <div className="upload-progress-bar" role="progressbar" aria-valuenow={progress} aria-valuemin={0} aria-valuemax={100}>
              <div style={{ width: `${progress}%` }} />
            </div>
          </div>
        )}

        {error && (
          <div className="error-banner" role="alert" style={{ marginBottom: 0 }}>
            {error}
          </div>
        )}

        {result && (
          <div className="stack" style={{ gap: 8 }}>
            <div className="row" style={{ flexWrap: 'wrap', gap: 8 }}>
              <Badge tone="ok">{result.uploaded.length} succeeded</Badge>
              {result.errors.length > 0 && (
                <Badge tone="danger">{result.errors.length} failed</Badge>
              )}
            </div>
            {result.errors.length > 0 && (
              <div className="stack" style={{ gap: 4 }}>
                {result.errors.map((item, idx) => (
                  <div key={`${item.filename}-${idx}`} className="row" style={{ gap: 6 }}>
                    <Badge tone="danger">{item.filename}</Badge>
                    <span className="view-subtitle" style={{ marginTop: 0 }}>
                      {item.message}
                    </span>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}

        <div className="confirm-actions" style={{ marginTop: 4 }}>
          <button type="button" className="button" disabled={uploading} onClick={onClose}>
            {result ? 'Done' : 'Close'}
          </button>
        </div>
      </div>
    </div>
  );
};
