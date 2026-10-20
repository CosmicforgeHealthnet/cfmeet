/**
 * V2 Quality Service
 * Handles quality metrics and sampling
 */

import apiV2 from "./api";
import type {
  QualitySampleBatch,
  QualityIngestResponse,
  ConferenceQualityView,
} from "@/types/v2";

export const qualityService = {
  /**
   * Submit quality samples
   */
  submitSamples: async (conferenceId: string, data: QualitySampleBatch) => {
    const response = await apiV2.post<QualityIngestResponse>(
      `/conferences/${conferenceId}/quality/samples`,
      data,
    );
    return response.data;
  },

  /**
   * Get conference quality summary
   */
  getQuality: async (conferenceId: string) => {
    const response = await apiV2.get<ConferenceQualityView>(
      `/conferences/${conferenceId}/quality`,
    );
    return response.data;
  },
};
