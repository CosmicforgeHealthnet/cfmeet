ARG PYTHON_IMAGE
FROM ${PYTHON_IMAGE}
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg && rm -rf /var/lib/apt/lists/*
RUN python -m pip install --disable-pip-version-check --no-cache-dir livekit==1.1.20
ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
USER 10001:10001
