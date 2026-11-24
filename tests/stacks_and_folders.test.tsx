// @vitest-environment jsdom
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { Lightbox } from '../src/components/Lightbox';
import { TimelineView } from '../src/components/views/TimelineView';
import { useAppStore } from '../src/store/useAppStore';
import type { AssetDetail, AssetSummary } from '../src/types/api';

const mockListAssets = vi.fn();
const mockListFolderSubfolders = vi.fn();
const mockGetAssetDetail = vi.fn();
const mockSetStackCover = vi.fn();
const mockRemoveFromStack = vi.fn();

vi.mock('../src/lib/api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/lib/api')>();
  return {
    ...actual,
    listAssets: (...args: unknown[]) => mockListAssets(...args),
    listFolderSubfolders: (...args: unknown[]) => mockListFolderSubfolders(...args),
    getAssetDetail: (...args: unknown[]) => mockGetAssetDetail(...args),
    setStackCover: (...args: unknown[]) => mockSetStackCover(...args),
    removeFromStack: (...args: unknown[]) => mockRemoveFromStack(...args),
  };
});

function makeAsset(overrides: Partial<AssetSummary>): AssetSummary {
  return {
    id: 'asset-1',
    folder_id: 'f-edited',
    kind: 'photo',
    capture_at_ms: 1_793_577_596_090,
    width: 6960,
    height: 4640,
    duration_ms: null,
    aspect_ratio: 1.5,
    is_favorite: false,
    deleted_at_ms: null,
    thumb_rel_path: 'aa/bb/asset-1.webp',
    thumb_ready: true,
    title: null,
    rating: null,
    gps_lat: null,
    gps_lon: null,
    geo_city: null,
    geo_country: null,
    camera_model: 'Canon EOS R7',
    stack_group_id: 'grp-1',
    stack_role: 'edited',
    stack_count: 3,
    stack_has_edited: true,
    stack_has_raw: true,
    ...overrides,
  };
}

function makeDetail(id: string, role: string, ext: string): AssetDetail {
  const members = [
    {
      asset_id: 'a-edited',
      folder_id: 'f-edited',
      folder_name: 'Edited Photos',
      rel_path: 'Bald Mountain/Alpenglow.jpg',
      filename: 'Alpenglow.jpg',
      extension: 'jpg',
      kind: 'photo' as const,
      stack_role: 'edited',
      is_cover: true,
      is_raw: false,
      width: 6960,
      height: 4640,
      file_size: 8_000_000,
      thumb_ready: true,
      capture_at_ms: 1_793_577_596_090,
      software: 'Adobe Photoshop Lightroom Classic 15.3',
    },
    {
      asset_id: 'a-raw',
      folder_id: 'f-dump',
      folder_name: 'Photo Dump',
      rel_path: 'Bald Mountain/RAW/4K1A1899.CR3',
      filename: '4K1A1899.CR3',
      extension: 'cr3',
      kind: 'photo' as const,
      stack_role: 'raw',
      is_cover: false,
      is_raw: true,
      width: 6960,
      height: 4640,
      file_size: 35_000_000,
      thumb_ready: true,
      capture_at_ms: 1_793_577_596_090,
      software: null,
    },
  ];
  return {
    summary: makeAsset({ id, stack_role: role, stack_count: 2 }),
    files: [
      {
        id: `file-${id}`,
        asset_id: id,
        folder_id: role === 'raw' ? 'f-dump' : 'f-edited',
        rel_path: role === 'raw' ? 'Bald Mountain/RAW/4K1A1899.CR3' : 'Bald Mountain/Alpenglow.jpg',
        file_role: role === 'raw' ? 'raw' : 'primary',
        mime_type: role === 'raw' ? 'image/x-canon-cr3' : 'image/jpeg',
        extension: ext,
        file_size: role === 'raw' ? 35_000_000 : 8_000_000,
        mtime_ms: 1_793_577_596_090,
        partial_blake3: `b3-${id}`,
        content_identifier: null,
        embedded_video_offset: null,
        embedded_video_length: null,
        is_missing: false,
      },
    ],
    photo_meta: {
      camera_make: 'Canon',
      camera_model: 'Canon EOS R7',
      lens_model: 'RF-S 10-18mm',
      focal_length_mm: 12,
      focal_length_35mm: 19,
      aperture_f: 4,
      shutter_speed_sec: 0.00625,
      iso: 100,
      orientation: 1,
      color_space: 'sRGB',
      gps_lat: null,
      gps_lon: null,
      gps_alt_m: null,
      geo_city: null,
      geo_admin1: null,
      geo_country: null,
      geo_country_code: null,
      taken_at_ms: 1_793_577_596_090,
      software: role === 'edited' ? 'Adobe Photoshop Lightroom Classic 15.3' : null,
    },
    video_meta: null,
    overrides: null,
    tags: [],
    histogram: null,
    album_ids: [],
    playback: {
      mode: 'image',
      direct_url: null,
      hls_url: null,
      live_video_url: null,
      has_raw: role === 'raw',
      preview_url: `/api/media/${id}/preview`,
      thumb_url: `/api/media/${id}/thumb`,
      original_url: `/api/media/${id}/original`,
    },
    stack_members: members,
  };
}

describe('Timeline Folder Picker & Lightbox Stack Version Switcher', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useAppStore.setState({
      user: {
        id: 'u-admin',
        username: 'admin',
        display_name: 'Ian',
        role: 'admin',
        avatar_color: '#4a9eff',
      },
      folders: [
        {
          id: 'f-edited',
          name: 'Edited Photos',
          root_path: '/photos/edited',
          volume_uuid: null,
          volume_label: null,
          is_managed_uploads: false,
          allow_disk_delete: false,
          is_offline: false,
          last_scanned_at_ms: null,
          asset_count: 12,
          created_at_ms: 1,
        },
        {
          id: 'f-dump',
          name: 'Photo Dump',
          root_path: '/photos/dump',
          volume_uuid: null,
          volume_label: null,
          is_managed_uploads: false,
          allow_disk_delete: false,
          is_offline: false,
          last_scanned_at_ms: null,
          asset_count: 40,
          created_at_ms: 2,
        },
      ],
      lightboxAssetId: null,
      lightboxAssetIds: [],
      libraryRevision: 0,
    });
  });

  it('filters timeline by watched folder, nested subfolder, breadcrumbs, and stack toggle', async () => {
    mockListAssets.mockResolvedValue({
      items: [makeAsset({ id: 'a-edited' })],
      next_cursor: null,
      total_count: 1,
    });
    mockListFolderSubfolders.mockResolvedValue({
      items: [
        { path: '2026', name: '2026', parent_path: null, depth: 0, asset_count: 10 },
        { path: '2026/hikes', name: 'hikes', parent_path: '2026', depth: 1, asset_count: 10 },
        { path: '2026/hikes/RAW', name: 'RAW', parent_path: '2026/hikes', depth: 2, asset_count: 5 },
      ],
    });

    render(<TimelineView />);
    await waitFor(() => {
      expect(mockListAssets).toHaveBeenCalledWith(
        expect.objectContaining({ collapse_stacks: true, folder_id: undefined }),
      );
    });

    // Open folder picker and select Edited Photos
    fireEvent.click(screen.getByRole('button', { name: /Folder filter: All Folders/i }));
    fireEvent.click(screen.getByRole('button', { name: /^Edited Photos/i }));
    await waitFor(() => {
      expect(mockListAssets).toHaveBeenLastCalledWith(
        expect.objectContaining({ folder_id: 'f-edited', path_prefix: undefined }),
      );
    });

    // Open picker again, expand Photo Dump subfolders, drill into 2026 -> hikes -> RAW
    fireEvent.click(screen.getByRole('button', { name: /Folder filter: Edited Photos/i }));
    fireEvent.click(screen.getByRole('button', { name: /Browse subfolders of Photo Dump/i }));
    await waitFor(() => expect(mockListFolderSubfolders).toHaveBeenCalledWith('f-dump'));

    fireEvent.click(screen.getByRole('button', { name: /Expand 2026/i }));
    fireEvent.click(screen.getByRole('button', { name: /Expand hikes/i }));
    fireEvent.click(screen.getByRole('button', { name: /^RAW/i }));

    await waitFor(() => {
      expect(mockListAssets).toHaveBeenLastCalledWith(
        expect.objectContaining({ folder_id: 'f-dump', path_prefix: '2026/hikes/RAW' }),
      );
    });

    // Unchecking stack toggle sends collapse_stacks=false
    fireEvent.click(screen.getByRole('button', { name: /Folder filter: Photo Dump \/ RAW/i }));
    fireEvent.click(screen.getByRole('checkbox'));
    await waitFor(() => {
      expect(mockListAssets).toHaveBeenLastCalledWith(
        expect.objectContaining({ collapse_stacks: false }),
      );
    });

    // Breadcrumb allows clearing back to All Folders
    fireEvent.click(screen.getByRole('button', { name: /Clear folder filter/i }));
    await waitFor(() => {
      expect(mockListAssets).toHaveBeenLastCalledWith(
        expect.objectContaining({ folder_id: undefined, path_prefix: undefined }),
      );
    });
  });

  it('renders Lightbox stack version switcher and toggles Edited <-> RAW with backslash shortcut', async () => {
    mockGetAssetDetail.mockImplementation((id: string) =>
      Promise.resolve(
        id === 'a-raw'
          ? makeDetail('a-raw', 'raw', 'cr3')
          : makeDetail('a-edited', 'edited', 'jpg'),
      ),
    );
    useAppStore.setState({
      lightboxAssetId: 'a-edited',
      lightboxAssetIds: ['a-edited', 'a-next'],
    });

    render(<Lightbox />);

    const rawBtn = await screen.findByRole('button', { name: 'RAW' });
    const editedBtn = screen.getByRole('button', { name: 'Edited' });
    expect(editedBtn.getAttribute('aria-pressed')).toBe('true');
    expect(rawBtn.getAttribute('aria-pressed')).toBe('false');

    // Click RAW -> switches displayed version in-place without changing timeline slot
    fireEvent.click(rawBtn);
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'RAW' }).getAttribute('aria-pressed')).toBe('true');
    });
    expect(useAppStore.getState().lightboxAssetId).toBe('a-edited');

    // Press backslash `\` -> instant Before/After back to Edited cover!
    fireEvent.keyDown(document, { key: '\\' });
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Edited' }).getAttribute('aria-pressed')).toBe(
        'true',
      );
    });
  });
});
