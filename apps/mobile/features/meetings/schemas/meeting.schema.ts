import { z } from "zod";

export const createMeetingSchema = z.object({
  title: z
    .string()
    .transform((v) => v.trim())
    .pipe(z.string().min(1, "Title is required").max(100, "Title must be at most 100 characters")),
  description: z
    .string()
    .transform((v) => v.trim())
    .pipe(z.string().max(500, "Description must be at most 500 characters"))
    .optional(),
  scheduled_at: z.string().min(1, "Date and time is required"),
  duration_minutes: z.number().min(15, "Minimum duration is 15 minutes").max(180, "Maximum duration is 3 hours"),
  is_private: z.boolean(),
});

export const joinMeetingSchema = z.object({
  display_name: z
    .string()
    .transform((v) => v.trim())
    .pipe(z.string().min(1, "Display name is required").max(50, "Display name must be at most 50 characters")),
  meeting_identifier: z
    .string()
    .transform((v) => v.trim())
    .pipe(z.string().min(1, "Meeting code is required")),
});

export const updateMeetingSchema = z.object({
  title: z
    .string()
    .transform((v) => v.trim())
    .pipe(z.string().min(1, "Title is required").max(100, "Title must be at most 100 characters"))
    .optional(),
  description: z
    .string()
    .transform((v) => v.trim())
    .pipe(z.string().max(500, "Description must be at most 500 characters"))
    .optional(),
  scheduled_at: z.string().optional(),
  duration_minutes: z.number().min(15).max(180).optional(),
  is_private: z.boolean().optional(),
});

export type CreateMeetingFormData = z.infer<typeof createMeetingSchema>;
export type JoinMeetingFormData = z.infer<typeof joinMeetingSchema>;
export type UpdateMeetingFormData = z.infer<typeof updateMeetingSchema>;
