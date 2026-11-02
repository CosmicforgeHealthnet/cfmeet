ARG PLAYWRIGHT_IMAGE
FROM ${PLAYWRIGHT_IMAGE}
WORKDIR /probe
RUN npm init -y && npm install --save-exact playwright@1.56.1
USER pwuser
