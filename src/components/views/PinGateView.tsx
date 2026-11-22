import React, { useEffect, useState } from 'react';
import { ApiClientError } from '../../lib/api';
import { isTauri } from '../../lib/tauri';
import { errorMessage, useAppStore } from '../../store/useAppStore';
import { Badge } from '../ui';
import { WindowsCaptionButtons, isWindowsPlatform } from '../WindowControls';

const nativeMacTitlebar =
  typeof navigator !== 'undefined' && /Mac/.test(navigator.platform);
const winDesktop = isTauri() && !nativeMacTitlebar && isWindowsPlatform();

const DIGIT_4 = /^\d{4}$/;
const DIGIT_6 = /^\d{6}$/;

export const PinGateView: React.FC = () => {
  const serverGatePassed = useAppStore((s) => s.serverGatePassed);
  const profiles = useAppStore((s) => s.profiles);
  const submitServerPin = useAppStore((s) => s.submitServerPin);
  const submitProfileLogin = useAppStore((s) => s.submitProfileLogin);

  const [forceServerStep, setForceServerStep] = useState(false);
  const [serverPin, setServerPin] = useState('');
  const [verifiedServerPin, setVerifiedServerPin] = useState<string | null>(null);
  const [selectedProfileId, setSelectedProfileId] = useState<string>('');
  const [profilePin, setProfilePin] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [countdownSecs, setCountdownSecs] = useState(0);

  const showProfileStep = serverGatePassed && !forceServerStep && (profiles?.length ?? 0) > 0;

  useEffect(() => {
    if (!profiles || profiles.length === 0) {
      setSelectedProfileId('');
      return;
    }
    setSelectedProfileId((prev) =>
      prev && profiles.some((p) => p.id === prev) ? prev : profiles[0].id,
    );
  }, [profiles]);

  useEffect(() => {
    if (countdownSecs <= 0) return;
    const timer = window.setInterval(() => {
      setCountdownSecs((prev) => (prev <= 1 ? 0 : prev - 1));
    }, 1000);
    return () => window.clearInterval(timer);
  }, [countdownSecs]);

  const handleApiError = (err: unknown) => {
    if (err instanceof ApiClientError && err.status === 429) {
      const secs = Math.max(1, err.retryAfterSecs ?? 5);
      setCountdownSecs(secs);
      setError(err.message || 'Too many failed attempts.');
      return;
    }
    setError(errorMessage(err));
  };

  const handleServerPinSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (countdownSecs > 0 || submitting) return;
    setError(null);

    if (!DIGIT_6.test(serverPin)) {
      setError('Enter the 6-digit Server PIN.');
      return;
    }

    setSubmitting(true);
    try {
      await submitServerPin(serverPin);
      setVerifiedServerPin(serverPin);
      setForceServerStep(false);
      setProfilePin('');
    } catch (err) {
      handleApiError(err);
    } finally {
      setSubmitting(false);
    }
  };

  const handleProfileLoginSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (countdownSecs > 0 || submitting) return;
    setError(null);

    if (!selectedProfileId) {
      setError('Select a profile to continue.');
      return;
    }
    if (!DIGIT_4.test(profilePin)) {
      setError('Enter your 4-digit Profile PIN.');
      return;
    }

    setSubmitting(true);
    try {
      await submitProfileLogin(selectedProfileId, profilePin, verifiedServerPin);
    } catch (err) {
      handleApiError(err);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div
      className={`auth-viewport${isTauri() && nativeMacTitlebar ? ' native-titlebar' : ''}${winDesktop ? ' windows-caption' : ''}`}
      data-tauri-drag-region
    >
      {winDesktop && (
        <div className="windows-auth-chrome" data-tauri-drag-region>
          <WindowsCaptionButtons />
        </div>
      )}
      <div className="auth-shell">
        <div className="auth-header">
          <img src="/app-icon.png" width={36} height={36} alt="" />
          <h1>TruongPhoto</h1>
          <p>
            {showProfileStep
              ? 'Choose your profile and enter your 4-digit PIN.'
              : 'Enter the 6-digit Server PIN to continue.'}
          </p>
        </div>

        {countdownSecs > 0 && (
          <div className="error-banner" role="alert" data-testid="rate-limit-countdown">
            Too many failed attempts. Try again in {countdownSecs}s.
          </div>
        )}

        {error && countdownSecs === 0 && (
          <div className="error-banner" role="alert">
            {error}
          </div>
        )}

        {!showProfileStep ? (
          <form
            className="card card-body stack"
            onSubmit={(e) => void handleServerPinSubmit(e)}
            noValidate
          >
            <label className="settings-field" style={{ marginBottom: 4 }}>
              <span>Server PIN</span>
              <input
                className="pin-input"
                type="password"
                inputMode="numeric"
                autoComplete="off"
                maxLength={6}
                placeholder="••••••"
                aria-label="Server PIN"
                value={serverPin}
                onChange={(e) => setServerPin(e.target.value)}
              />
            </label>
            <button
              type="submit"
              className="button button-primary"
              disabled={submitting || countdownSecs > 0}
            >
              {countdownSecs > 0
                ? `Locked (${countdownSecs}s)`
                : submitting
                  ? 'Verifying…'
                  : 'Unlock Server'}
            </button>
          </form>
        ) : (
          <form
            className="card card-body stack"
            onSubmit={(e) => void handleProfileLoginSubmit(e)}
            noValidate
          >
            <div className="profile-grid" role="radiogroup" aria-label="User profiles">
              {profiles?.map((profile) => {
                const selected = profile.id === selectedProfileId;
                return (
                  <button
                    key={profile.id}
                    type="button"
                    role="radio"
                    aria-checked={selected}
                    className={`profile-card ${selected ? 'is-selected' : ''}`}
                    onClick={() => {
                      setSelectedProfileId(profile.id);
                      setError(null);
                    }}
                  >
                    <div
                      className="profile-avatar"
                      style={{ background: profile.avatar_color || '#4a4a4a' }}
                    >
                      {profile.display_name.slice(0, 1).toUpperCase()}
                    </div>
                    <span style={{ fontWeight: 500 }}>{profile.display_name}</span>
                    <Badge tone={profile.role === 'admin' ? 'info' : 'muted'}>
                      {profile.role}
                    </Badge>
                  </button>
                );
              })}
            </div>

            <label className="settings-field" style={{ marginBottom: 4 }}>
              <span>Profile PIN</span>
              <input
                className="pin-input"
                type="password"
                inputMode="numeric"
                autoComplete="off"
                maxLength={4}
                placeholder="••••"
                aria-label="Profile PIN"
                value={profilePin}
                onChange={(e) => setProfilePin(e.target.value)}
              />
            </label>

            <button
              type="submit"
              className="button button-primary"
              disabled={submitting || countdownSecs > 0}
            >
              {countdownSecs > 0
                ? `Locked (${countdownSecs}s)`
                : submitting
                  ? 'Signing in…'
                  : 'Sign In'}
            </button>

            <button
              type="button"
              className="button button-quiet"
              onClick={() => {
                setForceServerStep(true);
                setProfilePin('');
                setError(null);
              }}
            >
              Back to Server PIN
            </button>
          </form>
        )}
      </div>
    </div>
  );
};
